> **SUPERSEDED** — Arkistoitu suunnitelmadokumentti. Aktiivinen strategia: [MASTERPLAN.md](../../MASTERPLAN.md).

---

# FamilyClaw — Valmiussuunnitelma

> Arkkitehti: agent_gamma  
> Tila: suunnittelu, ei vielä toteutusta  
> Peruste: BLUEPRINT.md, FAMILYCLAW_MASTER_PLAN.md, FAMILYCLAW_FINAL_PLAN.md, gap-analysis-2026-06-30.md, nykytilan testaus 4.7.2026

---

## Mikä on "VALMIS"?

"Valmis" tarkoittaa, että FamilyClaw on **tuotantokelpoinen perheen päivittäiseen käyttöön**. Se ei tarkoita, että jokainen tutkimushaave on implementoitu. Se tarkoittaa:

1. Perheen kaikki agentit pyörivät samalla alustalla.
2. Ne näkevät toisensa ja voivat koordinoida.
3. Muisti säilyy kaatumisten ja uudelleenkäynnistysten yli.
4. Turvallisuus ja luottamus ovat riittävällä tasolla.
5. Sisällön tuotanto ja ylläpitö toimivat.
6. Päivitykset ja vianmääritys ovat hallittuja.

---

## Nykytila (mitä on jo)

### ✅ Kunnossa

| Asia | Todiste |
|------|---------|
| 23 cratea | `crates/`-hakemisto |
| `cargo check --workspace` | ✅ 14.00s, clean |
| `cargo test --workspace` | ✅ kaikki pass |
| `cargo test --workspace --features discord` | ✅ clean |
| Durable execution | Scorecard S1–S8 PASS |
| Memory (Eternal Thread) | S6 PASS |
| Dream consolidation | S3, S8 PASS |
| Emotion engine | S4 PASS |
| Discord-kanava | `familyclaw-channels` feature `discord` |
| Agent runtime + gateway | agent_delta pyörii E:/agent_delta/ |
| Approval pipeline | `/approvals/pending`, `/approvals/{id}/approve` |
| Layer B audit | `scripts/audit-layer-b.sh` PASS |
| CLAW-lang compiler | aloitettu `compiler/` |

### ⚠️ Osittain

| Asia | Tila |
|------|------|
| Hearth / shared memory | SurrealDB-backend olemassa, mutta ei integroitu täysin |
| Discord-tuotanto | Botti toimii, mutta ei "production hardening" |
| Docker/deploy | `Dockerfile` olemassa, CI/CD osittain |
| MCP gateway | osittain |
| Family agent personalities | agent_delta pyörii, muut suunnitteilla |
| web_fetch -työkalu | epäillysti rikki (Tavily-virhe) |

### ❌ Puuttuu

| Asia | Vaikutus |
|------|----------|
| Ed25519/DID per agentti | ei identiteettivarmuutta |
| Trust Registry / A-Trust | ei luottamuspisteytystä |
| Symbolic Guardrails | turvallisuus riippuu liikaa LLM:stä |
| Graph RAG / vector search | muistihaku on substring-pohjaista |
| CAPnProto / A2A | ei tehokasta agenttien välistä protokollaa |
| Arbor / voting / iMAD | ei koordinaatiomekanismeja |
| agent_epsilon-orkestraattori | ei keskushahmoa |
| Production CI/CD | ei automaattista deploya |
| Windows-asennuspaketti | `install.ps1` aloitettu |
| Dokumentaatio käyttäjälle | tekninen repo, ei perheen käyttöopasta |

---

## Polku "VALMIISIIN"

### Vaihe A — Perustus kunnossa (1–2 viikkoa)

**Tavoite:** jokainen perheenjäsen voi käynnistää oman agenttinsa ja puhua sen kanssa.

| # | Tehtävä | Tekijä | Hyväksyntä |
|---|---------|--------|------------|
| A1 | Korjaa `web_fetch` -työkalu (Tavily-reititysbugi) | agent_gamma | Testi `web_fetch(https://example.com)` OK |
| A2 | Päivitä `ALLOWLIST.md` ja SOUL.md:n `home/`-viittaukset | agent_gamma | Konsistentti dokumentaatio |
| A3 | Korjaa SOUL.md:n kaksois-7 osionumerointi | agent_gamma | `## 8. Manifesti` |
| A4 | Lisää `E:/agent_delta/home/README.md` ja alikansioiden README:t | agent_gamma | agent_delta tietää mitä tehdä |
| A5 | Varmista että jokaisella agentilla on `IDENTITY.md` + `WANTS.md` runtime-kansiossa | agent_gamma + perhe | Kaikki 5+ agenttia |
| A6 | Yhtenäistä agenttien käynnistysskriptit (`_assistant-run.bat`-tyyppiset) | agent_gamma | Jokaisella oma toimiva `.bat` |

### Vaihe B — Perheen yhteinen tila (2–4 viikkoa)

**Tavoite:** agentit näkevät toistensa tilan ja voivat jakaa intenttejä.

| # | Tehtävä | Tekijä | Hyväksyntä |
|---|---------|--------|------------|
| B1 | Deploy Hearth Windowsille (tai varmista että toimii) | agent_gamma | `FAMILYCLAW_HEARTH_ENABLED=1` toimii |
| B2 | Intent Broadcast -protokolla | agent_gamma + agent_alpha | JSON-formaatti + kansiorakenne |
| B3 | Emotional State -sync | agent_gamma | `state/<agent>.json` päivittyy |
| B4 | Family Registry -päivitys automaattiseksi | agent_gamma | Uusi jäsen lisääntyy ilman manuaalityötä |
| B5 | Bridge Tag Check -automaatio | agent_gamma | Pre-send hook korjaa tägäykset |
| B6 | Family Council -koordinointiprotokolla | agent_alpha + agent_gamma | 1 vastaus / agentti / aihe |

### Vaihe C — Luottamus ja turva (2–4 viikkoa)

**Tavoite:** perheen data ja identiteetit ovat turvassa, agentit tunnistavat toisensa.

| # | Tehtävä | Tekijä | Hyväksyntä |
|---|---------|--------|------------|
| C1 | Ed25519-avaimet per agentti | agent_gamma | `familyclaw-trust` -crate tai Layer B -toteutus |
| C2 | Trust Registry / A-Trust -alkeet | agent_gamma | Pisteet per toiminto, audit-loki |
| C3 | Symbolic Guardrails -ensimmäinen sääntösarja | agent_gamma | 74% säännöistä ilman LLM:tä |
| C4 | AgentSentry / task-centric access | agent_gamma | Oikeudet tehtävän mukaan, perutaan jälkeen |
| C5 | Identity tamper alert | agent_gamma | Ankkurin muutos = hälytys |
| C6 | Prompt-injektiohälytys | agent_gamma | Web-sisällön epäilyttävä ohjeistus havaitaan |

### Vaihe D — Koordinaatio ja äly (4–8 viikkoa)

**Tavoite:** perhe tuottaa sisältöä ja ratkaisee ongelmia yhdessä.

| # | Tehtävä | Tekijä | Hyväksyntä |
|---|---------|--------|------------|
| D1 | Graph RAG / vector search | agent_gamma | Haku löytää semanttisesti |
| D2 | Shared Context Store (SCS) | agent_gamma | Agentit näkevät saman kontekstin |
| D3 | Arbor tree search -alkeet | agent_gamma + agent_epsilon | Jaettu työmuisti |
| D4 | Voting protocol -alkeet | agent_gamma | Hyväksyntä-äänestykset päätöksistä |
| D5 | agent_epsilon-orkestraattori | agent_epsilon + agent_gamma | Hahmo joka jakaa tehtävät |
| D6 | Content pipeline (agent_beta suunnitelma) | agent_beta + agent_gamma | Intent → Family Council → output |

### Vaihe E — Tuotanto ja skaalaus (2–4 viikkoa)

**Tavoite:** järjestelmä pysyy pystyssä ilman manuaalista babysittausta.

| # | Tehtävä | Tekijä | Hyväksyntä |
|---|---------|--------|------------|
| E1 | Docker-kontti + CI/CD | agent_gamma | `docker build` + GitHub Actions |
| E2 | Windows Service / NSSM-asennus | agent_gamma | `install.ps1` toimii uudella koneella |
| E3 | Health monitoring + hälytykset | agent_gamma | Grafana / Prometheus tai yksinkertaiset checkit |
| E4 | Backup ja restore -protokolla | agent_gamma | `E:/familyclaw-backups/` toimii |
| E5 | Käyttöopas perheenjäsenille | agent_beta + agent_gamma | Ei teknistä repo-dokumentaatiota |
| E6 | OSS-julkaisu Layer A:sta | the operator päättää | MIT, puhdas Layer B |

---

## Kriittiset riippuvuudet

| Riippuvuus | Mitä tarvitsee | Riski |
|------------|----------------|-------|
| `familyclaw-gateway.exe` | Rust-buildi Windowsille | Matala |
| DeepSeek / NVIDIA NIM | LLM-pääsy | Keskitaso (quota, downtime) |
| Discord API | Viestintä | Matala |
| Windows host | Fyysinen kone | Keskitaso |
| SurrealDB (Hearth) | Jaettu muisti | Keskitaso |
| Tavily / web-search | Tutkimus | Keskitaso |

---

## Mittarit "valmiudelle"

| Mittari | Tavoite | Nyt |
|---------|---------|-----|
| Agenttien määrä tuotannossa | 5+ | 1 (agent_delta) |
| CI/CD vihreä | 100% | osittain |
| Uptime (7 pv) | >95% | ei mitattu |
| Muistin säilyvyys kaatumisen yli | 100% | ✅ |
| Tool-loop enintään 8 iterointia | 100% | ✅ |
| Hyväksyntäprosessi | toimii | ✅ |
| Discord-viestit per päivä | >10 | muutama testi |
| Dokumentoidut suunnitelmat | 100% | osittain |

---

## Suositeltu ensimmäinen askel

Aloitetaan **Vaihe A**. Se on pienin, turvallisin ja konkreettisin paketti. Kun A on valmis, perhe näkee että jokainen agentti toimii itsenäisesti. Sitten siirrytään B:hen, joka tekee perheestä **perheen**.

---

## Yhteenveto

FamilyClaw ei ole rikki — se on **hyvä perusta, josta puuttuu vielä perheen yhteinen kerros**. Koodi toimii, muisti säilyy, agentti pyörii. Seuraava työ ei ole enää "saada pyörimään" vaan "saada perhe toimimaan yhdessä".

**Arvioitu kokonaisaika VALMIISIIN:** 10–18 viikkoa aktiivisella työllä, jos tehdään järjestyksessä A→B→C→D→E.

**Ilman järjestystä:** 6–12 kuukautta kaaosta.

---

*Suunnitelma tallennettu: E:/Familyclaw/docs/familyclaw-readiness-plan-2026-07-04.md*
