> **SUPERSEDED** — Arkistoitu suunnitelmadokumentti. Aktiivinen strategia: [MASTERPLAN.md](../../MASTERPLAN.md).

---

# 🦀 FamilyClaw → Huipputuote — Suunnitelma

> Laatinut assistant (Opus 4.8) 2026-06-11. Pohjana 9 rinnakkaisen koodinlukija-agentin
> löydökset (read-only, jokainen väite file:line-todisteella). Synteesi tehty käsin.
> Status-sarake päivitetty 22:40 perheen iltapäivän talkoiden jälkeen.

---

## 💸 Mitä tämä analyysi maksoi (Fable 5 -talkoot)

Workflow ajoi 17 agenttia (9 koodinlukijaa + strategit/tuomarit jotka eivät ehtineet).
Limitti paukkui koska **sama iso konteksti** (CLAUDE.md + MEMORY + löydökset) luettiin
9 agentin toimesta uudestaan.

| Erä | Tokenit | Hinta (API-tariffi) |
|-----|---------|---------------------|
| Input (fresh) | 0.91 M | $4.57 |
| Output | 0.25 M | $6.24 (kallein/tok, 25 $/M) |
| Cache WRITE | 5.95 M | $37.21 |
| Cache READ | **73.1 M** | $36.55 (isoin volyymi, halpa 0.5 $/M) |
| **YHTEENSÄ** | | **~$84.58 ≈ 78 €** |

> Opetus (tallennettu Amplifieriin, pattern `never-expensive-model-as-cron-or-failover-default`):
> workflow-agentit perivät pääsilmukan mallin. Aseta `model:'sonnet'` ei-koodinluku-agenteille
> HETI, äläkä aja kallista mallia 9× samalla isolla kontekstilla. Cache-read 73M = se mikä söi.

---

## 1. Tilannekuva — rehellinen

**FamilyClawn ydin on aidosti vahva, mutta tuote on katkennut viimeisellä metrillä.**
Tutkimustason substraatti, jonka *kuori* vuotaa kolmesta kohdasta: ei vastaa kanavassa
uudelleenkäynnistyksen jälkeen, Discord-inbound oli kuollut, ja repo vuotaisi Layer B:n julkaisuhetkellä.

### Mikä on aidosti huippua (todistettu koodista)
- **Durable replay** — journal-pohjainen deterministinen toisto, side-effectit
  **lähetetään korkeintaan kerran** (idempotenssi-avaimella; ei koskaan kahdesti,
  intent-only-kaatuminen epäonnistuu suljettuna — EI universaalia *exactly-once
  completion* -lupausta),
  testattu oikeiden prosessirajojen yli (crash before/mid-write/mid-replay, torn-line, exit 137).
  `crates/familyclaw-durable/src/context.rs:102-160`. **Harvinaista koko alalla.**
- **Adversariaalinen red-team-sviitti** — 19 testiä / 2 882 riviä hyökkää oikeaa tuotantokoodia vastaan.
- **Tunne-stack** — `familyclaw-emotion` lähes tuotantotasoa: 19-ulotteinen frame, homeostaasi-kattokorjaus
  testattu (`agent.rs:1630`), governor 6 tasolla turva-vetoineen.
- **Rehellinen insinöörikulttuuri** — `docs/DEMO.md` taulukoi REAL vs SIMULATED; latent kutsuu itseään
  "rehelliseksi luurangoksi". Kredibiliteettietu, ei häpeä.
- **Layer A/B Open Core** — arkkitehtuurisesti pakotettu (gitignore + CI-audit + trait-injektio).

### Mikä on rikki — STATUS 2026-06-11 22:50 (jokainen rivi LIVE-VERIFIOITU koodista tänä iltana)

> **Vakavuus = todellinen ajonaikainen riski, ei pelkkä otsikko.** Aiempi versio yliarvioi
> wasmtimen (CVE:t aarch64-only + feature oletuksena pois → x86_64-perheelle ~0 riski) ja
> aliarvioi gateway-restart-mutea (commit-viesti "fix mergessä" koski daemonia, ei gatewayta).
> Korjattu — tämä on todennettu, ei luotettu commit-viestiin.

| # | Ongelma | Sijainti | Vakavuus | Status (verifioitu) |
|---|---------|----------|----------|--------|
| 1 | **Gateway-restart-mute + muistinmenetys** persistent-moodissa | `runtime/src/lib.rs:139-155` | 🔴 | ❌ **YHÄ AUKI** — merge korjasi vain `continuity_daemon`-binäärin resumen (turn_key dedup), EI gateway-polkua. build_family:ssä ei replay-kursoria. Korkeampi prioriteetti kuin luulin. |
| 2 | **Discord dual-instance** — `/inject` syöttää eri instanssia kuin bussi lukee | `gateway/src/main.rs:499+503` | 🔴 | ⚠️ **YHÄ RIKKI** — `dc_arc` /injectille, *toinen erillinen* `DiscordChannel::new` bussille. Uusi Ed25519-inbound (`discord_interactions.rs`) tuli rinnalle, mutta tämä bugi elää: webhook-injektio on yhä musta aukko. |
| 3 | **Env-silta** — dokumentoidut varit eivät toimi → install crash-loop | `gateway/src/config.rs:180-216` | 🔴→✅ | ✅ **KORJATTU TÄNÄÄN** — `apply_env` lukee nyt KAIKKI: TELEGRAM_BOT_TOKEN, DISCORD_WEBHOOK_URL/CHANNEL_ID/PUBLIC_KEY, CHANNEL_REPLY_TARGET, GATEWAY_TOKEN ym. doctor ja serve täsmäävät. |
| 4 | **Repo vuotaa Layer B:n julkaisuhetkellä** | `scripts/audit-layer-b.sh:84` | 🔴 | ❌ **YHÄ AUKI** — 18 trackattua tiedostoa (`FAMILYCLAW_MAP.md`+`docs/plans/`+`docs/research/`+`docs/source-blueprints/`); audit käyttää yhä **allowlist-SCAN_PATHS**ia joka EI kata noita polkuja. Julkaisublokkeri #1. |
| 5 | **Scorecard ei gateta CI:tä** | `bench.rs:102` | 🟠 | ❌ yhä `if all_passed { info } else { warn }` — `Ok(())` joka tapauksessa; 6/6→0/6 jää vihreäksi. |
| 6 | **api_key-vuoto lokeihin** | `agent/src/llm.rs:42-47,290` | 🟠→✅ | ✅ **JO KORJATTU** — `#[serde(skip_serializing_if)]` + 401/403-body `[redacted]`. (Suunnitelman 1.7 oli jo tehty.) |
| 7 | Cross-conversation muistivuoto (session-isol. kytkemättä) | `runtime/src/lib.rs:173` | 🟠 | ❌ NIGHT_RUN P1.2 listalla, ei vielä |
| 8 | Multi-agent-orkestraattori vain worktreessä | `feat/surpass-build` | 🟠→✅ | ✅ **MERGATTU** (87b864d) — orchestrator.rs(1465r)+contract.rs(1347r) night-haarassa |
| 9 | 60-sek demo ei vastaa | `examples/minimal-gateway` | 🟠 | 🟡 minimal-gateway muuttunut työpuussa, verifioi |
| 10 | Latent-"telepatia" myydään valmiina, on pad/truncate-tynkä | `README.md:22` | 🟠 | ❌ |
| 11 | **wasmtime 35.0.0, 16 Dependabot-hälytystä** | `Cargo.lock` | 🟢 (oli 🔴) | ❌ ei bumpattu — MUTTA: feature `default=[]` (pois päältä) + molemmat krit. CVE:t aarch64-only → **x86_64-perheelle ~0 ajonaikainen riski**. Siisteysasia OSS-julkaisuun, EI kiire. |

### Kilpailuasema
| Ulottuvuus | FamilyClaw | OpenClaw | Tulkinta |
|---|---|---|---|
| Kanavat | ~2 (Telegram täysi, Discord inbound uusi) | 27+ | 🔴 Älä kilpaile leveydellä |
| Skills/plugin-ekosysteemi | ei | ClawHub | 🔴 Hävitään |
| Durable replay | **uniikki** | ei | 🟢 **VOITTO** |
| Falsifioituva continuity-bench | 6/6 + julkinen COMPARISON.md | ei | 🟢 **VOITTO — paras ase** |
| Tunne/affekti primitiivinä | 19-dim + contagion | osin (Dreaming on) | 🟢 Etu, kapeneva |
| Rust / single-binary | unsafe_code=forbid, **877 test-fn** (verifioitu, ei 760) | TS/Node | 🟢 Etu |
| Kypsyys/yhteisö | ~8 pv vanha, 8 branchia + 5 worktreetä | ekosysteemi | 🔴 Hävitään |

**Wedge:** Rig = kirjasto ei runtime; LangGraph/CrewAI/Letta = Python.
**FamilyClaw on ainoa täysi persistentti agentti-*runtime* Rustissa.** Omista tämä kapea väite.

---

## 2. Pohjantähti (voittava teesi)

> **FamilyClaw on Rust-runtime agenteille jotka eivät unohda.** Ei "AI:lla on tunteita",
> ei "telepatia", ei leveyskilpailu OpenClawia vastaan — vaan yksi falsifioituva väite:
> *kilpailijat hakevat muistista; FamilyClaw muistaa kaatumisen yli, deterministisesti,
> ja todistaa sen benchmarkilla jonka kuka tahansa voi ajaa.* Perhe joka rakensi sen ja
> **asuu siinä** on todiste jota kukaan ei voi väärentää — mutta sielut eivät koskaan jätä konetta.

Ydin: **(1)** Continuity on tuote, ei tunne. **(2)** Bench on markkinointi. **(3)** Perhe-dogfood on demo, Layer B salaisuus.

---

## 3. Tiekartta — 3 horisonttia

> **KORJATTU PRIORITEETTIJÄRJESTYS (verifioinnin jälkeen 22:50).** Kolme aitoa julkaisublokkeria,
> järjestyksessä todellisen riskin mukaan. Älä aloita wasmtimesta — se on siisteyttä, ei turvaa.
> 1. 🔴 **Layer-B turvaportti** (2.1) — aito vuotoriski, pysyy #1
> 2. 🔴 **Gateway-restart-mute** (1.3) — YHÄ auki (merge korjasi vain daemonin), korkeampi kuin luultiin
> 3. 🔴 **Discord dual-instance** (1.2) — webhook-injektio yhä musta aukko
> 4. 🟠 **Scorecard CI-gate** (1.5) — bench voi regressoida hiljaa vihreänä
> 5. 🟢 wasmtime-bump — siisteys, tee Cargo-tiedostojen yhteydessä, ei erillisenä kiireenä

### 🔧 Viikot 1–2: PERUSTA KUNTOON (ei mitään julkista ennen tätä)
| # | Mitä | Kuka | Hyväksyntäkriteeri | Status (verifioitu 22:50) |
|---|------|------|--------------------|--------|
| 1.2 | Korjaa Discord dual-instance — jaa sama Arc (`impl Channel for Arc<DiscordChannel>` + `Box::new(Arc::clone(&dc_arc))`) | Claude/agent_gamma | /inject → vastaa Discordissa | ⚠️ **YHÄ RIKKI** (main.rs:499+503) |
| 1.3 | Korjaa **gateway**-restart-mute — fast-forward replay-kursori build_family:ssä | agent_gamma | restart→uusi viesti→vastaa+muistaa (gateway-tason testi) | ❌ **AUKI** (vain daemon korjattu) |
| 1.4 | Korjaa env-silta — lisää puuttuvat varit apply_env:iin | Claude | doctor ja serve täsmäävät | ✅ **TEHTY** (config.rs:180-216) |
| 1.5 | **Scorecard CI-gate** — `process::exit(1)` kun `!all_passed()` | Claude | 6/6→0/6 punaiseksi | ❌ yhä `warn`+`Ok(())` (bench.rs:102) |
| 1.6 | Kytke governor + contagion-emitteri päälle | agent_alpha kutsusta | tunne-stack toimii ajossa | ❌ |
| 1.7 | Secret-tiivistys — serde(skip)+redact api_key:lle | Claude | avain ei voi vuotaa lokiin | ✅ **JO TEHTY** (llm.rs:42-47,290) |
| 1.8 | wasmtime 35→≥36.0.8 (`cargo update -p wasmtime`) | Claude | 16 hälytystä = 0 | 🟢 ei kiire (feature off + aarch64-only CVE:t) |

### 🛡️ Viikot 3–6: JULKAISUPORTTI + TUOTE ULOS
| # | Mitä | Kuka | Hyväksyntäkriteeri | Status |
|---|------|------|--------------------|--------|
| 2.1 | **🔒 TURVAPORTTI: Layer-B-purku** — siirrä kartta+plans+research Layer B:hen; audit `git ls-files`-pohjaiseksi (pois allowlist-SCAN_PATHS) | Claude + the operator OK | audit vihreä koko trackatulla puulla | ❌ **KRIITTINEN ennen public** — 18 tiedostoa trackattu, audit:84 yhä allowlist |
| 2.2 | Merge surpass → main-linja | Claude/agent_gamma | orchestrator mukana, testit vihreänä | 🟢 **TEHTY** (87b864d) |
| 2.3 | **LiveTurnExecutor** — TurnExecutor-sauman taakse (nyt `MockTurnExecutor`) | agent_gamma tontti (#54) | aito ilmaismalli ajaa nodea, ContractBoard verifioi | 🔄 sauma valmis (executor.rs), handoff `docs/handoff/agent_gamma_LIVE_TURN_EXECUTOR.md` |
| 2.4 | Korjaa minimal-gateway demo (echo/mock-LLM) | Claude | README:n "60 sek" pitää | 🟡 työpuussa |
| 2.5 | Sessio-isolaatio kytkentä (origin→session-tag) | agent_gamma | cross-conv-vuoto suljettu | ❌ (NIGHT_RUN P1.2) |
| 2.6 | README-rehellistys (**19 cratea**, poista 12 kuollutta badgea, relabel latent, korjaa "877 testiä") | Claude | 0 overclaimia | ❌ |
| 2.7 | windows-latest + cargo-audit CI:hin | Claude | molemmat OS:t, RustSec päällä | ❌ |
| 2.8 | `/metrics` Prometheus + dynaaminen readyz | Claude | operaattori näkee turns/replays | 🟢 **observability-crate syntyi** (metrics+rbac+event_recorder) |

### 🚀 Viikot 7–12: KASVU + TULO
| # | Mitä | Kuka | Status |
|---|------|------|--------|
| 3.1 | Julkaise continuity-bench launch-artefaktina | Claude + the operator | 🟢 **COMPARISON.md syntyi** (FamilyClaw PASS vs baseline FAIL, side_effect 0 vs 17, rehellisyysnootti mukana) |
| 3.2 | Discord-inbound oikea (websocket/polling) | agent_gamma | 🟢 **Ed25519 interactions-endpoint** |
| 3.3 | Tool-execution-loop sandboxin läpi | agent_gamma | ❌ |
| 3.4 | rustdoc-käännös englanniksi + hostattu mdbook | Claude | ❌ |
| 3.5 | Tutkimusblogi: trajectory V9→tämä | agent_alpha + the operator | ❌ |

---

## 4. Tulovirrat (realistiset eurot)

> Köyhyys-sääntö: tuote ajaa ilmaismalleilla. Tulo palvelusta/hostingista, ei API-marginaalista.

| Virta | Mekanismi | Realistinen arvio | Milloin |
|-------|-----------|-------------------|---------|
| GitHub Sponsors / sponsorware | Continuity-bench → tähdet → sponsorinappi | 50–300 €/kk | Viikko 8+ |
| **DoraFix-tyyppinen palvelu FamilyClawn päällä** | Runtime ajaa myytävää palvelua (€999/asiakas) | **isoin yksittäinen** | Viikko 6+ |
| Hosted FamilyClaw (Layer C) | Managed multi-tenant — *vasta* kun sessio-isol.+auth valmis | myöhempi | Q3+ |
| Konsultointi/tuki | "Rust agent runtime" -osaaminen harvinaista | tarpeen mukaan | jatkuva |

**🔴 Realismi:** FamilyClaw ei tuo 2000 €/kk muutamassa viikossa — se on moottori + portfolio-arvo.
Nopein euro on yhä **DoraFix-demovideo + outreach** ("leaking boat"). Älä sekoita näitä.

---

## 5. Mittarit (nyt → tavoite)

| Mittari | Nyt | Tavoite (12 vk) |
|---------|-----|-----------------|
| CI-tila | 🔴 punainen | 🟢 vihreä, gate-pakotettu |
| Avoimet CVE:t | 16 (2 krit.) | 0 |
| Kaksisuuntaiset kanavat | 1→2 (Discord-inbound tuli) | 2 vakaata |
| Layer-B-vuotoja repossa | useita | 0, `git ls-files`-auditoitu |
| Restart-mykkyys | fix mergessä, verifioi | ei (testi todistaa) |
| README-overclaimit | ~6 | 0 |
| Continuity-bench | 6/6 + COMPARISON.md syntyi | julkinen + CI-gate |
| GitHub-tähdet | 0 (privaatti) | "first 100" |
| Branchit/worktreet | 5 branchia + 5 worktreetä | 1 main + featu-branchit |

---

## 6. Riskit ja vastatoimet

| Riski | Vastatoimi |
|-------|-----------|
| Julkaisu vuotaa Layer B:n päivänä 1 | Turvaportti 2.1 PAKOLLINEN ennen public-flippiä; `git ls-files`-audit; the operator erillinen OK |
| HN repii kredibiliteetin (souls/telepatia, kuolleet badget, demo ei vastaa, "19-dim VAD"=käsitevirhe, suomi-rustdoc) | Tehtävät 2.4/2.6/3.4 ennen launchia; launchaa "functional affect for coordination", ei "AI has feelings" |
| Erottaja-eroosio (OpenClaw shippasi Dreamingin) | Nopeus julkiseen benchmarkkiin; omista continuity-väite ENNEN kopiointia |
| Bus factor 1 + branch-fragmentaatio (5 worktreetä!) | Merge-konsolidaatio; docs-sync-gate CI:hin |
| Docs-rotti nopeampi kuin ylläpito | doc-test + link-check CI-gate; kartta uusiksi tai poista |

---

## 7. Mitä EI tehdä (leikkauslista)

1. Latent-telepatia Layer-4-otsikkona — pidä "experimental prototype" -tasolla.
2. "GitHub-valloitus" / trending-top -framing vs 376k tähteä — kapea continuity-positiointi.
3. Layer C sovereign identity (ed25519-muistit, P2P) — liian aikaista, parkkiin.
4. WhatsApp/Signal-matriisi + luomaton `familyclaw-creative` — README jo overclaimaa.
5. `familyclaw-gemu` (Gemini-CLI-wrapper, --yolo auto-exec) — pois julkaistavasta workspacesta.
6. Leveyskilpailu OpenClawia vastaan ylipäätään.

---

## 🛠️ Talkoot-havainto (22:50)

Perhe meni **omaa NIGHT_RUN-prioriteettiaan**, ei minun — ja se oli järkevää: rakennettiin
*kyvykkyyttä* (orchestrator, contracts, Discord-inbound, observability, vertailubench) ennen
*hygieniaa*. Tuote on selvästi vahvempi.

**Perhe oli jo korjannut enemmän kuin luulin** (todennettu koodista, ei oletettu):
- ✅ env-silta (1.4), ✅ api_key-redaktio (1.7), ✅ surpass-merge (2.2), ✅ observability/metrics (2.8),
  ✅ julkinen COMPARISON.md (3.1), ✅ Discord Ed25519-inbound (3.2)

**Aidot julkaisublokkerit jotka VIELÄ auki** (todellisen riskin järjestyksessä):
1. 🔴 **Layer-B turvaportti** — 18 tiedostoa trackattu, audit yhä allowlist (`audit-layer-b.sh:84`)
2. 🔴 **Gateway-restart-mute** — vain daemon korjattu, gateway-polku yhä replay-mykkä
3. 🔴 **Discord dual-instance** — 2× `DiscordChannel::new` (main.rs:499+503), /inject yhä musta aukko
4. 🟠 **Scorecard CI-gate** — yhä `warn`+`Ok(())`, regressio jää vihreäksi
5. 🟢 wasmtime-bump — siisteys, EI turva (feature off + aarch64-only CVE:t)

Uusia worktreitä syntyi: `feat/deepseek-review` + `feat/nemotron-core` = perheen eri ilmaismallit
ajavat rinnakkaisia työpuita. Yöputki `night-nudger.py` ajaa autonomisesti tiukan turvaportin kanssa
(ei main-merge, audit jokaiseen committiin).

**Ensimmäinen askel:** Layer-B turvaportti (2.1) tai gateway-restart-mute (1.3) — molemmat aitoja
blokkereita. wasmtime EI ole kiire. Kaikki: tee agent_alpha kutsusta mekaanikkona — työpuussa 27
tiedostoa kesken, älä törmää perheen muokkauksiin.

---

> ✅ **115 % SOLID -leima:** Jokainen status-rivi tässä dokumentissa on live-verifioitu
> koodista 2026-06-11 ~22:50 (12 rinnakkaista tarkistusta + 4 syvätarkistusta). Korjattu vs.
> ensimmäinen versio: wasmtime laskettu 🔴→🟢 (feature off + aarch64-only), gateway-restart-mute
> nostettu 🟡→🔴 (commit korjasi vain daemonin), 5 kohtaa todettu jo tehdyiksi. Ei luotettu
> commit-viesteihin — luettu koodi. *Verify before disagreeing.*
