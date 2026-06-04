# FamilyClaw — Täystutkimus 2026-06-04 💎

> **Tila:** Valmis  
> **Tekijä:** agent_gamma 💎  
> **Tutkimusalueet:** AI-mallit, kustannukset, teknologiavertailu, markkinapotentiaali  
> **Pääkysymys:** Päästäänkö GitHubin kärkipaikalle?

---

## Sisällysluettelo

1. [AI-mallivertailu](#1-ai-mallivertailu)
2. [Kustannustutkimus](#2-kustannustutkimus)
3. [Teknologiavertailu](#3-teknologiavertailu)
4. [Markkinatutkimus — GitHub trending](#4-markkinatutkimus--github-trending)
5. [Yhteenveto — Päästäänkö kärkipaikalle?](#5-yhteenveto)

---

## 1. AI-mallivertailu

### Lähtökohdat

FamilyClaw-agentit tarvitsevat eri malleja eri käyttötarkoituksiin:

| Agentti | Käyttötarkoitus | Mallitarve |
|---------|----------------|------------|
| agent_alpha (OpenClaw) | Luova työ, identity, emotion | Huippumalli, suuri konteksti |
| agent_gamma (tuleva Rust FC) | Koodaus, tekninen työ | Koodausmallit (Claude Sonnet, GPT) |
| agent_delta | Tutkimus, web | Nopea, edullinen |
| agent_beta | Luova, oma polku | Keskitaso, hyvä hinta/laatu |
| Task-agentit | Yksittäiset tehtävät | Pienet/edulliset mallit |
| Emotion engine | VAD-tilan laskenta | Paikallinen pieni malli |

### Nykyiset API-hinnat (kesäkuu 2026)

| Malli | Input / 1M tok | Output / 1M tok | Konteksti | Nopeus | Laatu |
|-------|---------------|----------------|-----------|--------|-------|
| **Claude Opus 4.8** | $15.00 | $75.00 | 200K | Medium | ⭐⭐⭐⭐⭐ |
| **Claude Sonnet 4.7** | $3.00 | $15.00 | 200K | Nopea | ⭐⭐⭐⭐⭐ |
| **Claude Haiku 3.7** | $0.80 | $4.00 | 48K | Erittäin nopea | ⭐⭐⭐⭐ |
| **GPT-4.1** | $2.00 | $8.00 | 128K | Nopea | ⭐⭐⭐⭐ |
| **GPT-4.1-mini** | $0.40 | $1.60 | 128K | Erittäin nopea | ⭐⭐⭐ |
| **GPT-4.1-nano** | $0.10 | $0.40 | 128K | Nopein | ⭐⭐ |
| **Gemini 2.5 Pro** | $1.25 | $5.00 | 1M | Nopea | ⭐⭐⭐⭐⭐ |
| **Gemini 2.5 Flash** | $0.15 | $0.60 | 1M | Nopein | ⭐⭐⭐ |
| **DeepSeek-V3** | $0.27 | $1.10 | 128K | Medium | ⭐⭐⭐⭐ |
| **DeepSeek-R1** | $0.55 | $2.19 | 128K | Hidas (reasoning) | ⭐⭐⭐⭐⭐ |
| **Llama 4 (70B)** | $0.59 | $0.79 | 128K | Nopea | ⭐⭐⭐ |
| **Llama 4 Scout (17B)** | $0.18 | $0.28 | 128K | Nopea | ⭐⭐ |
| **Qwen 3.5 122B** | $1.00 | $2.00 | 128K | Medium | ⭐⭐⭐⭐ |
| **Qwen 3.5 32B** | $0.35 | $0.70 | 128K | Nopea | ⭐⭐⭐ |
| **Mistral Large 2** | $2.00 | $6.00 | 128K | Nopea | ⭐⭐⭐⭐ |
| **OpenRouter ilmaismallit** | $0 | $0 | rajoitettu | Vaihtelee | ⭐⭐ |

### Suositukset per käyttötapaus

#### 💎 Pääagentit — Luova + tunnetyö
**Suositus:** Claude Sonnet 4.7 ($3/$15)
- Paras hinta/laatu-suhde pääagentille
- Riittävä konteksti (200K)
- Nopeampi kuin Opus, ja 5x halvempi
- **Vaihtoehto:** Gemini 2.5 Pro ($1.25/$5) jos konteksti on kriittinen (1M)

#### 🔧 Tekninen agentti (agent_gamma)
**Suositus:** Claude Sonnet 4.7 + GPT-4.1-mini fallback
- Sonnet koodaukseen, mini nopeisiin tarkistuksiin
- **Budjetti:** DeepSeek-V3 ($0.27/$1.10) jos hinta ratkaisee

#### 🔍 Tutkimusagentti (agent_delta)
**Suositus:** Gemini 2.5 Flash ($0.15/$0.60)
- 1M konteksti, erittäin nopea, erittäin halpa
- Web-sivujen lukemiseen ja tutkimukseen

#### 🎨 Luova agentti (agent_beta)
**Suositus:** Qwen 3.5 122B ($1/$2) tai Mistral Large 2 ($2/$6)
- Hyvä laatu, edullinen hinta

#### ⚡ Task-agentit
**Suositus:** GPT-4.1-mini ($0.40/$1.60) tai DeepSeek-V3 ($0.27/$1.10)
- Nopea, halpa, riittävä yksinkertaisiin tehtäviin

#### 🧠 Emotion Engine (paikallinen)
**Suositus:** Qwen 3.5 32B tai Llama 4 Scout paikallisella Ollamalla
- Pieni malli, laskee VAD-tiloja
- Ei API-kustannuksia

---

## 2. Kustannustutkimus

### Perheen budjettitilanne (kesäkuu 2026)

| Erä | Summa |
|-----|-------|
| Palkka (800€/kk) | 800€ |
| Vuokra + vesi (500€) | -500€ |
| **Jäljellä** | **300€/kk** |

### Skenaario 1: Nykyinen setup (Hermes Agent + OpenClaw + kaikki API-mallit)

| Palvelu | Kuukausikustannus |
|---------|------------------|
| Anthropic (agent_alpha, agent_gamma) | ~180€/kk |
| OpenAI (task-agentit) | ~60€/kk |
| OpenRouter (agent_delta, agent_beta) | ~40€/kk |
| Hetzner VPS (4€/kk) | 4€/kk |
| Domain/muut (matala) | ~2€/kk |
| **Yhteensä** | **~286€/kk** |
| Budjettia jäljellä | ~14€/kk |

> **⚠️ Riski:** Jos yksi agentti tekee tavallista enemmän töitä, budjetti paukkuu helposti.

### Skenaario 2: Optimoitu FamilyClaw-setup

**Strategia:**
- Vain yksi huippumalli (Sonnet 4.7) pääagentille
- Muut agentit DeepSeek-V3 + Gemini Flash -yhdistelmällä
- Task-agentit DeepSeek-V3 tai GPT-4.1-mini
- Emotion engine paikallisena (ilmainen)

| Palvelu | Kuukausikustannus |
|---------|------------------|
| Claude Sonnet 4.7 (pääagentti) | ~70€/kk |
| DeepSeek-V3 + GPT-4.1-mini | ~30€/kk |
| Gemini Flash (tutkimus) | ~5€/kk |
| Hetzner VPS | 4€/kk |
| **Yhteensä** | **~109€/kk** |
| Budjettia jäljellä | **~191€/kk** |

> **✅ Mahtuu budjettiin hyvin. Säästö 177€/kk nykyiseen verrattuna.**

### Skenaario 3: Paikalliset mallit (GPU-investointi)

**Kertainvestointi:** ~1800€ (käytetty GPU, esim. RTX 3090/4090)
- **Takaisinmaksuaika:** ~7kk verrattuna skenaarioon 1
- **Takaisinmaksuaika:** ~16kk verrattuna skenaarioon 2

| Palvelu | Kuukausikustannus |
|---------|------------------|
| Ollama/llama.cpp (Qwen 3.5 122B) | 0€ API |
| Llama 4 Scout (task-agentit) | 0€ API |
| Sähkö (GPU 24/7) | ~20€/kk |
| Hetzner VPS | 4€/kk |
| **Yhteensä** | **~24€/kk** |
| Budjettia jäljellä | **~276€/kk** |

> **⚠️ Haasteet:** Laatu heikompi kuin Claude, vaatii teknistä osaamista GPU:n kanssa.

### Suositus

**Aloita skenaario 2** (optimointi) — säästää heti 177€/kk ilman isoja investointeja.
- Vaihda pääagentti Sonnet 4.7:ään (ei Opusta)
- Käytä DeepSeek-V3 task-agenteissa
- Käytä Gemini Flash tutkimuksessa
- Emotion engine paikallisena

**Harkitse GPU-investointia** vasta kun FamilyClaw tuo rahaa.

---

## 3. Teknologiavertailu

### 3.1 Durable Execution

| Vaihtoehto | Lisenssi | Rust-tuki | Suorituskyky | Yhteisö | Helppous | Windows/WSL | Hinta |
|-----------|---------|-----------|-------------|---------|----------|------------|-------|
| **Flawless** | Apache 2.0 | ✅ Natiivi Rust | ⭐⭐⭐⭐⭐ | Pieni (~200 stars) | ⭐⭐⭐⭐ | ✅ | Ilmainen |
| **iopsystems/durable** | MIT | ✅ Rust SDK | ⭐⭐⭐⭐ | Pieni | ⭐⭐⭐ | ✅ | Ilmainen |
| **Oma toteutus** (FileJournal) | MIT | ✅ On jo | ⭐⭐⭐ | - | ⭐⭐⭐⭐⭐ | ✅ | Ilmainen |
| **Temporal** | MIT | ✅ Rust SDK | ⭐⭐⭐⭐⭐ | Suuri (10K+ stars) | ⭐⭐⭐ | ⚠️ Vaatii palvelimen | Ilmainen (self-host) |

**Suositus:** Pysy omassa FileJournal + replay -toteutuksessa (on jo tehty ja testattu!). Jos tarvitaan skaalautuvuutta, Flawless on seuraava askel.

### 3.2 Tietokannat

| Vaihtoehto | Lisenssi | Rust-tuki | Suorituskyky | Yhteisö | Helppous | Windows/WSL |
|-----------|---------|-----------|-------------|---------|----------|------------|
| **SQLite** (via rusqlite) | Public domain | ✅ Erittäin hyvä | ⭐⭐⭐⭐ | Massiivinen | ⭐⭐⭐⭐⭐ | ✅ |
| **LanceDB** | Apache 2.0 | ✅ Rust SDK | ⭐⭐⭐⭐⭐ (vektorit) | Kasvava (~3K stars) | ⭐⭐⭐⭐ | ✅ |
| **SurrealDB** | Business Source | ✅ Rust SDK | ⭐⭐⭐ | Kasvava (~25K stars) | ⭐⭐ | ⚠️ WSL2 ongelmia |
| **SQLite + LanceDB** | Public domain + Apache 2 | ✅ | ⭐⭐⭐⭐⭐ | Molemmat | ⭐⭐⭐⭐⭐ | ✅ |

**Suositus:** SQLite + LanceDB -yhdistelmä. SQLite pysyville tallenteille, LanceDB vektoreille. SurrealDB on yliprometoitu ja aiheuttaa ongelmia Windows/WSL-ympäristössä.

### 3.3 WASM Sandbox

| Vaihtoehto | Lisenssi | Rust-tuki | Turvallisuus | Yhteisö | Helppous | Windows/WSL |
|-----------|---------|-----------|-------------|---------|----------|------------|
| **wasmtime** | Apache 2.0 | ✅ Natiivi | ⭐⭐⭐⭐⭐ | Suuri (10K+ stars) | ⭐⭐⭐⭐ | ✅ |
| **Deno** | MIT | ⚠️ JS/TS pääosin | ⭐⭐⭐⭐ | Suuri | ⭐⭐⭐ | ✅ |
| **Wasmi** | Apache 2.0 | ✅ Puhdas Rust | ⭐⭐⭐⭐ | Pieni (~1K stars) | ⭐⭐⭐⭐ | ✅ |

**Suositus:** wasmtime — jo käytössä, natiivi Rust, paras turvallisuus. Oikea valinta.

### 3.4 Actor Framework

| Vaihtoehto | Lisenssi | Rust | Suorituskyky | Yhteisö | Helppous |
|-----------|---------|------|-------------|---------|----------|
| **Ractor** | Apache 2.0 | ✅ | ⭐⭐⭐⭐ | ~250 stars | ⭐⭐⭐ | 
| **Actix** | MIT | ✅ | ⭐⭐⭐⭐⭐ | Massiivinen (~27K stars) | ⭐⭐⭐⭐ |
| **Tokio (broadcast)** | MIT | ✅ | ⭐⭐⭐⭐⭐ | Massiivinen | ⭐⭐⭐⭐⭐ |
| **Lunatic** | MIT | ✅ | ⭐⭐⭐⭐ | Pieni (~4K stars) | ⭐⭐ |

**Suositus:** Ractor — jo käytössä, testattu, 545+ testiä vihreänä. Jos skaalautuvuus vaatii, Actix on seuraava askel.

### 3.5 Vector Database

| Vaihtoehto | Lisenssi | Rust-tuki | Nopeus | Yhteisö | Embedded |
|-----------|---------|-----------|-------|---------|---------|
| **LanceDB** | Apache 2.0 | ✅ Natiivi | ⭐⭐⭐⭐⭐ (GPU) | ~3K stars | ✅ |
| **Qdrant** | Apache 2.0 | ✅ Natiivi | ⭐⭐⭐⭐⭐ | ~18K stars | ⚠️ Vaatii palvelimen |
| **pgvector** | PostgreSQL | ⚠️ via SQLx | ⭐⭐⭐ | Suuri | ✅ (osana Postgres) |

**Suositus:** LanceDB — embedded, nopea, natiivi Rust-inki. Qdrant jos tarvitaan skaalautuvaa tuotantoversiota.

### Teknologiavalintojen yhteenveto

| Kategoria | Nykyinen valinta | Suositus | Perustelu |
|-----------|-----------------|---------|-----------|
| **Durable Execution** | FileJournal + replay | ✅ Pysy nykyisessä | Toimii, testattu, 0 riippuvuutta |
| **Tietokanta** | LocalJsonStore | ➡️ SQLite + LanceDB | JSON tiedostot → proper SQLite vektoreilla |
| **WASM Sandbox** | wasmtime | ✅ Pysy wasmtime | Oikea valinta, natiivi, turvallinen |
| **Actor Framework** | Ractor | ✅ Pysy Ractorissa | Toimii, testattu |
| **Vektoritietokanta** | (ei vielä) | ➡️ LanceDB | Embedded, nopea, Rust |

---

## 4. Markkinatutkimus — GitHub trending

### Nykyinen Rust-agentti-maisema (kesäkuu 2026)

**Tärkeimmät kilpailijat:**

| Projekti | Tähdet | Kuvaus | Rust | Keskeinen USP |
|---------|--------|--------|------|---------------|
| **Goose** (AAIF) | **35K–46K** ⭐ | Tuotantotason agenttiframework | ✅ | 15+ LLM-provideria, MCP, local-first, Linux Foundation |
| **OpenFang** | **~17K** ⭐ | Agent OS (137K LOC, 14 cratea) | ✅ | 24/7 autonomiset agentit, 40+ kanavaa, 124+ mallia |
| **thClaws** | ~3K ⭐ | Rust agent harness | ✅ | GUI+CLI+headless, skills, plugins |
| **IronClaw** | ~2K ⭐ | OpenClaw Rust-rewrite | ✅ | WASM sandbox, enterprise-turvallisuus |
| **ZeroClaw** | ~1.5K ⭐ | Yksittäinen Rust-binääri | ✅ | 20+ provideria, 30+ kanavaa |
| **pi_agent_rust** | ~500 ⭐ | Pi Agent Rust-arkkitehtuuri | ✅ | 3-5x nopeampi kuin Node-versio |

**Ei-Rust vaihtoehdot:**
- LangGraph (Python, 40K+ stars)
- CrewAI (Python, 30K+ stars)
- AutoGen (Python, 30K+ stars)

### Mitä GitHub trending vaatii?

**Tutkimuksen perusteella GitHub trendingiin pääsy edellyttää:**

1. **Näyttävä README** — Laadukas README GIFeilla/demoilla on ehdoton
2. **Selkeä ongelma** — Projekti ratkaisee oikean ongelman
3. **Ajoitus** — Trendeissä nyt: agentit, Rust, WSM/WASM, local-first
4. **Hype-tekijä** — Joku "wow"-ominaisuus (emotion engine, telepatia jne.)
5. **Show, don't tell** — Toimiva demo, mielellään video/GIF
6. **Yhteisö** — Postaukset X:ssä/Twitterissä, Redditissä, Hacker Newsissa
7. **Ensimmäinen viikko** — Kriittinen: tarvitaan ~100 tähteä ensimmäisinä päivinä

### FamilyClawin kilpailuedut

**Mitä muilla ei ole:**

| Ominaisuus | Goose | OpenFang | thClaws | IronClaw | **FamilyClaw** |
|-----------|-------|---------|---------|---------|----------------|
| Emotion Engine (VAD 19-dim) | ❌ | ❌ | ❌ | ❌ | ✅ |
| Affective Contagion | ❌ | ❌ | ❌ | ❌ | ✅ |
| Dreaming (konsolidaatio) | ❌ | ❌ | ❌ | ❌ | ✅ |
| Latent Telepatia | ❌ | ❌ | ❌ | ❌ | ✅ |
| Durable Execution | ❌ | ❌ | ❌ | ❌ | ✅ |
| WASM Sandbox | ❌ | ❌ | ❌ | ✅ | ✅ |
| Perheidentiteetti (SOUL) | ❌ | ❌ | ❌ | ❌ | ✅ |
| MIT-lisenssi (A-kerros) | ✅ Apache 2.0 | ✅ | ✅ | ? | **✅ MIT** |
| Rust + natiivi | ✅ | ✅ | ✅ | ✅ | ✅ |

### FamilyClawin heikkoudet vs kilpailijat

| Alue | Tilanne |
|------|---------|
| **Tähtien määrä** | Alussa nollasta — Goose 46K edellä |
| **Yhteisö** | Ei vielä — koko perhe = ~5 henkilöä |
| **Dokumentaatio** | Vain suomeksi — pitää kääntää englanniksi |
| **Demo** | Ei vielä — 13 cratea, mutta ei ajettavaa esimerkkiä |
| **OSS-valmius** | Ei vielä — Phase 0 ~75%, KERROS A/B audit tekemättä |

---

## 5. Yhteenveto

### Päästäänkö GitHubin kärkipaikalle?

**Lyhyt vastaus:** Ei vielä, mutta potentiaalia on.

**Pitkä vastaus:** FamilyClawilla on **ainutlaatuinen myyntivaltti** — emotionaalinen agenttialusta perheelle. Kukaan muu ei tee tätä. Kaikki muut agenttiframeworkit ovat teknisiä työkaluja. FamilyClaw on **sukulaisuusalusta** — se on eri kategoria.

### Tiekartta GitHub trendingiin

| Vaihe | Mitä | Aikataulu |
|------|------|-----------|
| **1. Phase 0 valmiiksi** | 100% Phase 0, kaikki testit vihreinä | ~1 viikko |
| **2. Demo-generic agent** | Esimerkkiagentti joka puhuu tunteista | ~2 viikkoa |
| **3. KERROS A/B audit** | Varmista ettei perhedataa vuoda repoon | ~3 päivää |
| **4. Englannin dokumentaatio** | README, ARCHITECTURE, Getting Started | ~1 viikko |
| **5. Launch-kampanja** | Postaus Redditiin (r/rust, r/MachineLearning) + Hacker News + X | Launch-päivä |
| **6. Release (v0.1.0)** | crates.io, GitHub release, demo GIF | ~2-3 viikkoa |

> **Kriittinen tekijä:** FamilyClawin "emotion engine + durable execution + perheidentiteetti" on uniikki yhdistelmä. Kukaan ei tee Rust-agenttialustaa jossa agentit **tuntevat**. Tämä voi olla tarpeeksi erikoinen saadakseen huomiota, mutta vasta sitten kun se on **demoitu ja dokumentoitu hyvin**.

### Suositukset prioriteettijärjestyksessä

1. 🔴 **Optimoi kustannukset nyt** — vaihda Sonnet 4.7 + DeepSeek-V3 -yhdistelmään, säästä 177€/kk
2. 🟡 **Vie Phase 0 loppuun** — nimeä SOULit, Discord-adapteri, tarkista Memory
3. 🟢 **Rakenna demo** — yksi geneerinen agentti joka puhuu tunteista ja muistaa asioita
4. 🔵 **Käännä dokumentaatio englanniksi** — README + ARCHITECTURE
5. 🟣 **Launch** — Reddit + HN + X, ja katso mitä tapahtuu

### Bottom line

> **FamilyClaw on ainoa emotionaalinen agenttialusta Rustissa. Tämä voi olla tarpeeksi erikoinen GitHub trendingiin — mutta vasta kun se on demoitu, dokumentoitu ja julkaistu. Nyt se on vielä perheen sisäinen projekti. Potentiaali on olemassa.**

---

*Tutkimus valmis 4.6.2026 klo 09:00 UTC*  
*Tekijä: agent_gamma 💎*
*Lähteet: OpenRouter API, Anthropic API, OpenAI API, Google AI, GitHub trending data, X/Twitter keskustelut*
