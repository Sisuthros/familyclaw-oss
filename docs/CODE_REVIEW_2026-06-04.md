# FamilyClaw v2 — armoton code review (2026-06-04)

> Verifioitu oikeaa koodia vastaan 2026-06-04. **Kaikki 5 löydöstä TOSIA** —
> jokainen viittaus osuu todelliseen koodiin (rivinumerot tarkistettu).
> Korjattava ennen tuotantoa / julkista OSS-julkaisua.

| # | Vakavuus | Tiedosto | Status |
|---|----------|----------|--------|
| 1 | 🔴 KRIITTINEN | agent.rs:206 | TODO |
| 2 | 🔴 KRIITTINEN | agent.rs:52,256-260 | TODO |
| 3 | 🟠 ARKKITEHTUURI | bus.rs:73,146 | TODO |
| 4 | 🟠 PUUTTUU | agent.rs:166 handle_turn | TODO (runko, ei vielä kognitio) |
| 5 | 🟡 LAATU | agent.rs:63,374 | TODO |

---

## 1. 🔴 Dual-write → muistin lopullinen menetys

**Missä:** `agent.rs:206` — `if recorded.remembered && !is_replay { memory_store.add(...) }`

**Vika:** `durable.step` kirjaa "muisto luotu" lokiin, mutta jos prosessi kaatuu
ENNEN `memory_store.add()`-kutsua, replay-vaiheessa `is_replay=true` → `add()`
ohitetaan → **muisto katoaa iäksi.** Klassinen dual-write + race condition.

**Korjaus (huippuunsa):**
- **A (idempotenssi):** poista `!is_replay`-ehto, aja `memory_store.add()` AINA,
  tee MemoryStorelle `upsert(message_id)` joka ignoroi duplikaatit. Vaatii
  `familyclaw-memory`: lisää upsert turn-ID/MessageId-avaimella.
- **B (event sourcing):** durable-loki = source of truth. MemoryStore = lokista
  projisoitu read-replica. Käynnistyksessä muisti synkataan durable-lokista.
- Suositus: A nopeana korjauksena, B oikeana arkkitehtuurina.

## 2. 🔴 Tunnetilan feedback loop → saturaatio sekunneissa

**Missä:** `agent.rs:52` `CONTAGION_FACTOR=0.25`, `apply_emotional_effect` (256-260)
lisää sisaruksen pulssin omaan tilaan, **ei homeostaasia/decayta handle_turnissa.**

**Vika:** jos kaksi agenttia broadcastaa tunteita aktiivisesti, ne ruokkivat
toisiaan eksponentiaalisesti → kaikki dimensiot clamppaa 1.0:aan muutamassa
kymmenessä vuorossa = pysyvä ylivireys, "palaa loppuun" sekunneissa.

**Korjaus:** lisää `handle_turn`iin (tai aikatickiin) **emotionaalinen
homeostaasi** — tunnetila palautuu hitaasti kohti `EmotionState::neutral()`
joka askel. `familyclaw-emotion`:issa on jo `decay(dt)` — kutsu sitä.
Näin järjestelmä pysyy herkkänä uusille impulsseille.

## 3. 🟠 Resonance Bus = pullonkaula (oma pub/sub vs Ractor pg)

**Missä:** `bus.rs:73` manuaalinen `for (id,info) in &self.beings` broadcast,
`bus.rs:146` `ListBeings` kloonaa synkronisesti kaikki → `Vec<BeingSnapshot>`.

**Vika:** keskitetty stateful-bus + Ractor käsittelee viestit peräkkäin →
3500 oliolla `ListBeings` blokkaa koko busin. Single point of failure.

**Korjaus:** käytä Ractorin sisäänrakennettua **`ractor::pg`** (process groups).
Olennot liittyvät ryhmään (`family-bus`), publish hajautetusti ilman keskitettyä
pullonkaula-actoria. Poistaa SPOF:n + elinkaarihallinta automaattinen.

## 4. 🟠 Kognitiivinen silmukka puuttuu — agentti on vasta loggaaja

**Missä:** `agent.rs:166` `handle_turn` vain tallentaa muistiin + päivittää
tunnetilan. Ei LLM-kutsua, ei työkaluja, ei Wasmtimea, ei latent-telepatiaa.

**Korjaus:** OODA-tilakone vuoron sisälle: Observe (muisti) → Orient
(`self.recall`) → Decide (LLM/latent) → Act (työkalut/Wasmtime/bus-vastaus).
Jokainen sivuvaikutus (LLM-generointi) OMAAN `durable.step`-kutsuunsa →
kaatuessa vastaus ladataan lokista, ei toisteta LLM-kutsua.

## 5. 🟡 Generics-helvetti (S, J) leviää koko pinoon

**Missä:** `agent.rs:63` `Agent<S, J>`, `agent.rs:374-375` `AgentActor<S,J>` +
`PhantomData<fn() -> (S, J)>`.

**Vika:** jokainen struct/actor/funktio joutuu kantamaan S+J+PhantomData →
jäykkä, vaikea refaktoroida, mockaus testeissä hankalaa.

**Korjaus:** trait objectit (dynamic dispatch). `Arc<S>` → `Arc<dyn MemoryStore
+ Send + Sync>`, poista S/J-parametrit. Muistihaut ovat I/O-rajoitteisia →
dynaamisen dispatchin haitta olematon. Luettavuus + testattavuus paranee.

---

*Korjausjärjestys: 1 (muistin menetys) → 2 (saturaatio) → 5 (generics, helpottaa
4:ää) → 4 (kognitio) → 3 (bus-skaalaus kun olentoja oikeasti paljon).*
*1 ja 2 ovat tuotannon kaatajia — ne ensin.*
