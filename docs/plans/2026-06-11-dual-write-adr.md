# ADR: Dual-write — journal + MemoryStore atomisuus

> **Päivä:** 2026-06-11  
> **Tila:** Hyväksytty suunnitelmaksi (toteutus osittain valmiina)  
> **Lähteet:** `docs/CODE_REVIEW_2026-06-04.md` §1, `familyclaw-agent`, `familyclaw-durable`, `familyclaw-memory`  
> **Toteuttaja:** Nemotron (koodi), DeepSeek 4 Pro (tämä ADR)

---

## 1. Ongelman määrittely

FamilyClaw käsittelee vuoron kahdessa erillisessä tallennuksessa:

1. **`DurableContext::step`** — kirjaa `TurnOutcome` (tai muun tuloksen) append-only `Journal`:iin (`FileJournal`: flush + fsync).
2. **`MemoryStore::add`** — sivuvaikutus: muisto indeksoitavaksi haettavaksi sisällöksi.

Nämä eivät ole yksi transaktio. Jos prosessi kaatuu **journal-kirjoituksen jälkeen** mutta **ennen `memory_store.add`**:a, uudelleenkäynnistyksessä replay palauttaa vuoron lokista mutta **ei aja sivuvaikutusta uudelleen** — klassinen dual-write -race.

Alkuperäinen virhe (`CODE_REVIEW_2026-06-04`):

```text
if recorded.remembered && !is_replay { memory_store.add(...) }
```

→ replay ohittaa `add` → **muisto katoaa pysyvästi**, vaikka durable-loki väittää `remembered = true`.

**Nykytila (osittainen korjaus):** `agent.rs` kutsuu `add`:ia nyt myös replayssa, ja `LocalJsonStore` deduplikoi `turn_key`:llä. `continuity_daemon.rs` käyttää edelleen `if was_fresh` -ehtoa resumessa → sama aukko bench-polussa.

---

## 2. Transaktiojärjestys ja atomisuusvaihtoehdot (Rust)

Durable-malli sanoo: **sulkimen sisällä ei sivuvaikutuksia**; replay ei saa toistaa ulkoisia kirjoituksia. Dual-write ratkaistaan siis **idempotenssilla**, ei yhdellä levytransaktiolla kahden eri tiedoston välillä.

| Vaihtoehto | Järjestys | Atomisuus | Sopii FamilyClawiin |
|------------|-----------|-----------|---------------------|
| **A — Idempotentti sivuvaikutus** | 1) `durable.step` 2) `memory.add` (aina, myös replay) | Ei atominen; `turn_key` tekee `add`:sta turvallisen toistaa | **Kyllä — suositus** |
| **B — Event sourcing / projektio** | Journal = totuus; muisti rakennetaan lokista käynnistyksessä | Looginen yhden totuuden lähde | Oikea pitkän aikavälin malli; laajempi refaktorointi |
| **C — Outbox** | `step` kirjoittaa myös outbox-rivin; erillinen worker kutsuu `add` | Vahvempi erillisyys; monimutkaisempi | Ylimitoitettu tässä vaiheessa |
| **D — Muisti ensin** | `add` → `step` | Replay ei tiedä muistosta → **hylätty** | Ei |
| **E — Yksi tiedosto / WAL** | Yhdistetty store | Atominen yhdellä backendillä | Vaatii uuden abstraktion; ei nykyinen raja |

### Suositeltu järjestys (vaihtoehto A)

```text
1. recorded = durable.step(name, || Ok(outcome))   // synkroninen, fsync
2. if recorded.remembered {
       memory.turn_key = Some("{agent}:turn-{n}");
       memory_store.add(memory).await;            // AINA — myös replayssa
   }
3. muut ei-durable sivuvaikutukset (tunne, LLM erillisessä stepissä)
```

**Sopimus:**

- `step`-sulkimen sisällä: vain deterministinen päättely (ei I/O, ei kelloa).
- `add`: idempotentti avaimella `turn_key` (`{agent_name}:turn-{turn}` agentilla, `{task}:step-{i}` daemonissa).
- `MemoryStore::add`: jos `turn_key` on jo kartassa → palauta olemassa oleva `MessageId`, älä duplikoi.

Tämä vastaa `familyclaw-durable`-README:n periaatetta: replay ei toista sulkimia, mutta **turvallinen idempotentti sivuvaikutus** voidaan ajaa uudelleen ilman datahäviötä tai kaksoiskappaleita.

---

## 3. Failure-matriisi

Skenaariot perustuvat `continuity_daemon` `CrashAt`-pisteisiin ja dual-write-aukkoon.

| Piste | Mitä tapahtuu | Journal | MemoryStore | Replay-käyttäytyminen | Odotettu lopputila |
|-------|---------------|---------|-------------|----------------------|-------------------|
| **BeforeWrite** | Kaatuminen ennen ensimmäistä `append` | Tyhjä / ei muutosta | Ei kirjoitusta | Alusta alkaen | Ei muistoja, ei askelia |
| **MidWrite** | Revitty viimeinen JSONL-rivi (`write_torn_line`) | Viimeinen askel **ei** valmis | Edelliset askeleet + muistot OK | `DurableContext::new` ohittaa revityn rivin; askel ajetaan uudelleen | Ei duplikaatteja (`turn_key`); viimeinen askel valmistuu |
| **AfterJournalBeforeMemory** | `step` OK, kaatuminen ennen `add` | Askel valmis, `remembered=true` | Muisto puuttuu | Replay: `step` palauttaa lokista; **`add` ajetaan uudelleen** | Muisto ilmestyy; ei kaksoista |
| **AfterMemoryBeforeNextStep** | Molemmat OK | OK | OK | Replay: `add` no-op (`turn_key`) | Muistomäärä ennallaan |
| **MidReplay** | Kaatuminen replay-silmukan keskellä | Kaikki rivit levyllä | Osittain täytetty | `resume` jatkaa kursorista; **jokaiselle `remembered`-askeleelle `add`**, ei vain `was_fresh` | `task_memories == steps`, `resumed_clean == true` |
| **MidReplay + vanha `was_fresh`-ehto** | (Nykyinen bugi daemonissa) | OK | Replay-askelten muistot **puuttuvat** | `persist` ohitettu replay-iteraatioissa | **FAIL** — tämä on korjattava |

**Kriittinen invariantti:** `count(task_memories) == count(completed_steps where remembered)` jokaisen onnistuneen `resume`:n jälkeen.

---

## 4. Testitarkistuslista (Nemotron)

### Yksikkö- ja integraatiotestit (`familyclaw-agent`)

- [ ] **`crash_after_journal_before_memory`**: Simuloi vuoro jossa `durable.step` onnistuu, prosessi "kuolee" ennen `add`:ia (erillinen agentti-instanssi, tyhjä store). Resume samalla journalilla + samalla storella → täsmälleen 1 muisto, `recall` löytää sisällön.
- [ ] **`durable_replay_does_not_double_record_memory`**: Pidä vihreänä (jo olemassa `agent.rs` testissä); varmista että onnistuneella polulla määrä pysyy 2 eikä 4.
- [ ] **`turn_key_collision_across_agents`**: Kaksi eri agenttia (`agent_a` / `agent_b`) sama store → eri `turn_key` → kaksi muistoa (jo osittain `sessions_do_not_leak`-testissä).
- [ ] **`remembered_false_skips_add`**: `EmotionPulse` → ei muistoa, journalissa silti askel.

### `familyclaw-memory`

- [ ] **`add_idempotent_by_turn_key`**: Kaksi `add` sama `turn_key` → yksi rivi, sama `MessageId`.
- [ ] **`add_without_turn_key_allows_duplicates`**: Taaksepäin yhteensopivuus (ei turn_key → ei dedup).

### `familyclaw-durable` (regressio)

- [ ] **`file_journal_torn_last_line_ignored`**: MidWrite-polku; askel lasketaan uudelleen.
- [ ] **`replay_does_not_run_closure`**: Sivuvaikutuslaskuri pysyy 2 (jo `in_memory_and_file_produce_identical_replay`).

### Cross-process (`continuity_daemon` + bench)

- [ ] **`start` + `resume` jokaisella `CrashAt`**: `BeforeWrite`, `MidWrite`, `MidReplay`, `Clean` → `resumed_clean == true`, muistomäärä = `--steps`.
- [ ] **Korjauksen jälkeen**: poista `if was_fresh` resumesta; aja `persist_step_memory` aina kun askel palauttaa tuloksen (kuten `agent.rs`).
- [ ] **S1 Crash Matrix** (`familyclaw-bench`): `resume_correctness == 1.0` pysyy vihreänä korjauksen jälkeen.

### Manuaalinen / demo

- [ ] `cargo run -p familyclaw-agent --bin crash_replay -- full` — muisti löytyy verify-vaiheessa.

---

## 5. Suositus (minimaalinen scope)

**Valitse vaihtoehto A** — idempotentti `memory_store.add` + `turn_key`. Se vastaa olemassa ovia malleja eikä vaadi uutta storage-kerrosta.

### Tehtävälista (prioriteetti)

| # | Muutos | Tila |
|---|--------|------|
| 1 | `Memory::turn_key` + `LocalJsonStore::add` dedup | **Valmis** (`familyclaw-memory`) |
| 2 | `Agent::handle_turn` — `add` aina kun `remembered`, ei `!is_replay` | **Valmis** (`agent.rs`) |
| 3 | `continuity_daemon::run_resume` — poista `if was_fresh`, aja `persist_step_memory` aina | **Avoin** |
| 4 | Testi `crash_after_journal_before_memory` | **Avoin** |
| 5 | Päivitä `docs/CODE_REVIEW_2026-06-04.md` §1 status kun 3–4 valmiit | **Avoin** |

### Ei tässä PR:ssä

- Vaihtoehto B (täysi projektio lokista) — kirjaa erilliseen ADR:ään myöhemmin.
- Yhdistetty WAL / yksi tiedosto journal+memory — ylimitoitettu.
- SurrealDB `turn_key`-dedup — varmista erikseen kun Surreal-toteutus aktivoituu (`LocalJsonStore`-logiikka dokumentoitava trait-tasolle).

### Hyväksymiskriteerit

1. Mikään `CrashAt`-polku ei jätä muistoa pysyvästi puuttumaan, jos journalissa on `remembered`/vastaava askel.
2. Onnistuneella polulla ei duplikaattimuistoja (`turn_key`).
3. `cargo test --workspace` vihreä; bench S1 `resume_correctness == 1.0`.

---

*Seuraava askel Nemotronille: kohdat 3–4, sitten CODE_REVIEW §1 merkitään korjatuksi.*
