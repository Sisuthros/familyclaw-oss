# FamilyClaw — Masterplan

> **Strategian yksi totuudenlähde.** Tekninen tila: [STATUS.md](STATUS.md).
> Kohderyhmä ja adoption-gate: [docs/USERS.md](docs/USERS.md).
> Kaupallinen tarjonta: [docs/COMMERCIAL_OFFER.md](docs/COMMERCIAL_OFFER.md).
>
> Tämä dokumentti korvaa kaikki aiemmat ristiriitaiset suunnitelmat
> (`HUIPPUTUOTE_*`, `FAMILYCLAW_MASTER_PLAN`, `FAMILYCLAW_FINAL_PLAN`,
> `KEHITYSSUUNNITELMA`, `docs/familyclaw-readiness-plan-*`, `docs/V1_ROADMAP_DESIGN`).
> Ne säilytetään todistearvona: [docs/archive/](docs/archive/).

**Päivitetty:** 2026-07-06 · **Versio:** v1.2.0 (workspace)

---

## 1. Teesi ja positiointi

**FamilyClaw on Rust-runtime agenteille, joiden ulkoiset sivuvaikutukset selviävät
kaatumisesta korkeintaan kerran.**

Yksi falsifioituva väite: kilpailijat *hakevat* muistista; FamilyClaw *muistaa
kaatumisen yli* deterministisesti ja todistaa sen benchmarkilla, jonka kuka tahansa
ajaa yhdellä komennolla.

**Rehellinen takuun raja:** at-most-once *dispatch* ulkoisille sivuvaikutuksille
kaatumisen yli (idempotenssi-avain: ei koskaan kahdesti; intent-only-ikkunassa
epäonnistuu suljettuna). Tämä **ei** ole lupaus universaalista "exactly-once
completion" -valmistumisesta.

**Kategoria:** ei kirjasto (Rig), ei Python-liima (LangGraph/CrewAI/Letta), ei
leveyskilpailu (OpenClaw). FamilyClaw on *runtime-substraatti* — ainoa täysi
persistentti agentti-runtime Rustissa, jossa crash-safe external dispatch on
benchmarkattu oikeaa kilpailijaa vastaan.

**Kolme roolia yhdelle teesille** (ei kolmea erillistä tuotetta):

| Rooli | Tarkoitus | Mittari |
|-------|-----------|---------|
| **Bench on markkinointi** | OSS-läpimurto, uskottavuus | 1 ulkoinen `familyclaw serve` -ajo |
| **Reliability-palvelu on rahavirta** | Offer A/B, founding pilots | 1 maksava asiakas |
| **Perhe-dogfood on syvyys ja demo** | Layer B, koordinaatio, jatkuvuus | 5+ agenttia tuotannossa |

---

## 2. Kenelle

**Älä käytä**, jos agenttisi vain lukee ja tiivistää. Pysy Pythonissa.

**Käytä**, jos agenttisi *muuttaa maailmaa ja kaatuminen maksaa oikeaa rahaa tai
luottamusta* — migraatiot, pilviresurssien purku, hyvitykset.

### Kolme nimettyä profiilia

1. **Yksinäinen dev, joka paloi uudelleenajettuun migraatioon** — checkpoint ≠
   exactly-once; tarvitsee at-most-once dispatchin migraatiovaiheeseen.
2. **Infra-tiimi, jolla on autonominen cost-cleanup-agentti** — kaksinkertainen
   teardown kaatumisen jälkeen on outage.
3. **Fintech-tinkeröijä, jonka agentti myöntää hyvityksiä** — kaksinkertainen
   hyvitys on duplicate payout.

Lisätiedot: [docs/USERS.md](docs/USERS.md).

---

## 3. Mikä on aidosti valmista

Älä lue tätä osiota uudelleen — lue [STATUS.md](STATUS.md). Tiivistelmä:

| Kyvykkyys | Tila | Todiste |
|-----------|------|---------|
| Durable crash-replay | ✅ | Scorecard S1, `familyclaw-durable` |
| At-most-once external dispatch | ✅ | S1, LangGraph-bench |
| Eternal Thread -muisti + dream | ✅ | S2–S3, S6, S8 |
| Provenance gate | ✅ | S7 |
| Resonance Bus (affekti) | ✅ | S4 |
| Live multi-agent orchestration | ✅ | `orchestration_live.rs` |
| WASM sandbox e2e | ✅ | `sandbox_integration.rs` |
| Action/Skill runtime | ✅ | `familyclaw-actions` (270+ testiä) |
| Continuity scorecard | ✅ 8/8 | `cargo run -p familyclaw-bench --bin bench -- all` |
| LangGraph-vertailu | ✅ | `bench-competitors/langgraph/RESULTS.md` |

**Testipinta:** ~1680 testiä, 23 cratea, CI vihreä (`--all-features` mukaan lukien).

---

## 4. Kolme horisonttia (prioriteettijärjestys)

### Horisontti 1 — Julkaisukelpoinen v1.0 + git-konsolidointi

**Tavoite:** yksi puhdas `main`-linja ja vihreä julkaisuportti ennen mitään julkista.

| # | Tehtävä | Hyväksyntäkriteeri |
|---|---------|-------------------|
| H1.1 | Git-konsolidointi | **Merge valmis 2026-07-06:** `feat/expo-commercial-foundation` → `main`. Kartta: [docs/GIT_CONSOLIDATION.md](docs/GIT_CONSOLIDATION.md) |
| H1.2 | Layer-B turvaportti | `audit-layer-b.sh` skannaa `git ls-files` (ei allowlistia); `docs/archive/` karanteenissa — **PASS 2026-07-06** |
| H1.3 | Gateway-restart-mute | `Agent::resume_live()` + `gateway_restart_*` testi — **PASS 2026-07-06** |
| H1.4 | Discord dual-instance | Yksi `Arc` + `SharedDiscordChannel` — **korjattu** (verifioi `/inject` manuaalisesti) |
| H1.5 | Scorecard CI-gate | `bench all` → `Err` kun epäonnistuu; CI-job `all-features` — **lisätty 2026-07-06** |
| H1.6 | Rehellisyysportti | README "Should you use this?" + [docs/LAUNCH.md](docs/LAUNCH.md) — **2026-07-06** |

### Horisontti 2 — OSS-läpimurto: bench aseena

**Tavoite:** skeptikko kloonaa, ajaa yhden komennon, näkee todisteen.

**Launch-playbook:** [docs/LAUNCH.md](docs/LAUNCH.md) (Show HN / r/rust -luonnokset, checklist).

**Launch-artefakti:**

```bash
git clone https://github.com/Sisuthros/familyclaw
cd familyclaw
cargo run -p familyclaw-bench --bin bench -- compare
# + LangGraph-vertailu:
cd bench-competitors/langgraph && python crash_harness.py cycle --crash-point before_write
```

**Kanavat:** Show HN, r/rust — otsikko: *"Rust agent runtime joka selviää kaatumisesta
— tässä benchmark vs LangGraph."*

**Adoption-gate (ylittää kaiken muun):**

> Vähintään yksi ulkoinen henkilö ajaa `familyclaw serve` omassa repossaan ja raportoi.

Tähdet ovat sivutuote; tämä numero on ainoa, joka todistaa tuotteen.

### Horisontti 3 — Rahavirta + perheen koordinaatio (rinnakkain)

**B — Rahavirta**

| Tarjonta | Hinta | Sisältö |
|----------|-------|---------|
| Offer A: Reliability Review | 750–1500 € | Yhden workflow'n failure-mode -kartta |
| Offer B: Reliability Sprint | 1500–3500 € | 5 pv, yksi workflow crash-safe + approval + audit |

Yksityiskohdat: [docs/COMMERCIAL_OFFER.md](docs/COMMERCIAL_OFFER.md).

**Rehellisyys:** FamilyClaw-moottori ≠ suora cashflow. DoraFix-tyyppinen erillinen
palvelu voi rahoittaa R&D:tä; runtimen validointi = bench + ulkoinen käyttäjä,
**ei** DoraFix-liikevaihto.

**D — Koordinaatio ja äly** (kapeutettu teesin alle)

Vain ominaisuudet, jotka syventävät continuity-tarinaa:

| # | Tehtävä | Milloin |
|---|---------|---------|
| D1 | Hearth / jaettu muisti tuotannossa | Horisontti 3 alku |
| D2 | Semantic recall (vain recall-gaten jälkeen) | Kun fixture näyttää Hit@k > keyword |
| D3 | Live multi-agent laajennettu | Kun Horisontti 2 adoption-gate täyttyy |
| D4 | Perheen agentit (5+) samalla alustalla | Layer B, ei repoon |

Kaikki muu (CAPnProto, DID/VC, Arbor, voting, 3D-perception) → v2-visio.

---

## 5. Tulovirrat

| Virta | Mekanismi | Realistinen arvio | Prioriteetti |
|-------|-----------|-------------------|--------------|
| Offer A/B | Reliability Review + Sprint | 750–3500 €/asiakas | **1** |
| GitHub Sponsors | Bench-kredibiliteetti | 50–300 €/kk | 3 |
| Erillinen palvelu (DoraFix tms.) | Rahoittaa R&D:tä | vaihtelee | 2 (erillinen validointi) |
| Hosted multi-tenant (Layer C) | Vain session-isol. + auth jälkeen | myöhempi | 4 |

---

## 6. v2-visio ja Layer B -raidat

Nämä eivät kuulu v1:n kriittiselle polulle. Yksi rivi kukin — inspiroi, älä
lupaa.

| Raida | Kuvaus | Lähde |
|-------|--------|-------|
| CAPnProto-viestintä | −75 % overhead vs JSON | ZooMPC |
| Graph RAG / semanttinen haku | 0 LLM-kutsua retrieval-polussa | MemoryOS, FORGE |
| Trust Registry / A-Trust | Luottamuspisteytys per toiminto | AgentTrust |
| Symbolic Guardrails | 74 % säännöistä ilman LLM:ää | TAKO |
| Orchestrator-agentti | Keskushahmo tehtävien jakoon | Layer B |
| Latent telepathy | Send-side research, aina text fallback | `familyclaw-latent` |
| Growth loop (apply-polku) | Approval-gated self-modification | `familyclaw-growth` |
| Cryptographic identity (Layer C) | Signed memories, P2P trust | Post-v1 |
| Claw language compiler | Kokeellinen spike, ei workspacessa | `compiler/` |

---

## 7. Mitä EI tehdä

1. **Ei leveyskilpailua OpenClawia vastaan** — 27 kanavaa vs meidän 2 on häviö;
   omista continuity-wedge.
2. **Ei "semantic recall" -markkinointia** ennen recall-gaten läpäisyä.
3. **Ei silent self-modification** — growth-loop vain approval-gated.
4. **Ei latent-telepatiaa Layer-4-otsikkona** — experimental prototype.
5. **Ei WhatsApp/Signal-matriisia** v1:ssä.
6. **Ei `familyclaw-gemu`** julkaistavassa workspacessa.
7. **Ei kalliinta mallia cron/failover-oletuksena** (dokumentoitu €78 post-mortem).
8. **Ei 7-kerros/12-pilari-arkkitehtuuria** v1:n kriittisellä polulla.
9. **Ei Layer B -dataa repoon** — koskaan.
10. **Ei "live multi-agent shipped" -overclaimia** ilman live-integraatiotestiä.

---

## 8. Mittarit

| Mittari | Nyt | Tavoite (12 vk) |
|---------|-----|-----------------|
| Continuity scorecard | 8/8 PASS | 8/8 CI-gated |
| LangGraph-bench | FamilyClaw 0 vs LG 1/2 | Julkinen + toistettava |
| Adoption-gate | 0 ulkoista ajajaa | **≥1** |
| Maksava asiakas (Offer A/B) | 0 | **≥1** founding pilot |
| Agentteja tuotannossa (Layer B) | 1 | 5+ |
| Layer-B-vuotoja repossa | verifioi | 0 |
| CI-tila | vihreä | vihreä, gate-pakotettu |
| README-overclaimit | tarkista | 0 |
| Git-haarat | 12+ | `main` + ≤3 elävää featurea |

**Yksi adoption-gate ylitse muiden:**

> Vähintään yksi ulkoinen henkilö ajaa `familyclaw serve` omassa repossaan ja raportoi.

---

## Arkistoitu suunnitteluhistoria

| Dokumentti | Arkistoitu |
|------------|------------|
| `HUIPPUTUOTE_SUUNNITELMA.md` | [docs/archive/](docs/archive/HUIPPUTUOTE_SUUNNITELMA.md) |
| `FAMILYCLAW_HUIPPUTUOTE_SUUNNITELMA_2026-06-18.md` | [docs/archive/](docs/archive/) |
| `FAMILYCLAW_MASTER_PLAN.md` | [docs/archive/](docs/archive/) |
| `FAMILYCLAW_FINAL_PLAN.md` | [docs/archive/](docs/archive/) |
| `KEHITYSSUUNNITELMA.md` | [docs/archive/](docs/archive/) |
| `docs/familyclaw-readiness-plan-2026-07-04.md` | [docs/archive/](docs/archive/) |
| `docs/V1_ROADMAP_DESIGN.md` | [docs/archive/](docs/archive/) |

---

*The wedge is the bench; the moat is the boundary; the discipline is the honesty.*
