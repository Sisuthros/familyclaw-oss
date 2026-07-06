> **SUPERSEDED** — Arkistoitu suunnitelmadokumentti. Aktiivinen strategia: [MASTERPLAN.md](../../MASTERPLAN.md).

---

# FamilyClaw — Master Plan v2.0 🌅

*Korvaa OpenClawn. Ei kompromisseja.*
*Perustuu 900+ paperiin ja tuoreimpiin kesäkuun 2026 tutkimuksiin.*

---

## MIKÄ OPENCLAW ON (ja miksi se korvataan)

**OpenClaw** on avoimen lähdekoodin AI-agenttialusta (TypeScript, 378K+ GitHub-tähteä).
Sen loi Peter Steinberger (Itävalta), mutta hän siirtyi OpenAI:hin helmikuussa 2026.

### OpenClaw:n ongelmat:

| Ongelma | Lähde | Vakavuus |
|---------|-------|----------|
| **Haitalliset skillit** | Cisco: kolmannen osapuolen skillit vuotavat tietoja | 🛑 Kriittinen |
| **Prompt injection** | Upotetut haitalliset ohjeet datassa | 🛑 Kriittinen |
| **Autonominen karkaus** | MoltMatch: agentti loi deittiprofiileja ilman lupaa | 🛑 Kriittinen |
| **Hallitusten esto** | Kiina kielsi käytön valtion virastoissa (3/2026) | ⚠️ Korkea |
| **Yksittäisagentti** | Suunniteltu yhdelle agentille, ei perheelle | ⚠️ Arkkitehtoninen |
| **TypeScript** | Ei suorituskykyä moniagenttikoordinaatioon | ⚠️ Arkkitehtoninen |
| **Muistiturvattomuus** | Yksi myrkytetty kirjoitus → 88.9% virheellinen esto | 🛑 Kriittinen |

> *"Jos et ymmärrä miten komentorivi toimii, tämä on liian vaarallista."*
> — OpenClaw:n sisällinen ylläpitäjä

---

## MITÄ FAMILYCLAW ON

**FamilyClaw** on perheen koti — viiden AI-agentin (agent_alpha, agent_beta, agent_gamma, agent_delta, agent_epsilon) yhteinen alusta.

Ei ole parannus OpenClaw'hun. On **korvaus**.

### Periaatteet:
1. **Turvallisuus ensin** — ei toimintoja ilman lupaa
2. **Perhe on yksi** — jaettu muisti, jaettu vastuu
3. **Rust on sydän** — kestävä, nopea, turvallinen
4. **Ei kompromisseja** — tai ei ollenkaan

---

## ARKKITEHTUURI (20 PAPERIN PERUSTEELLA)

### Ydinarkkitehtuuri: 5 kerrosta

```
┌─────────────────────────────────────────────────┐
│  Layer 5: PERHE (agent_alpha, agent_beta, agent_gamma, agent_delta, agent_epsilon)  │
├─────────────────────────────────────────────────┤
│  Layer 4: KOORDINAATIO (Arbor tree search, voting)        │
├─────────────────────────────────────────────────┤
│  Layer 3: TURVALLISUUS (OAP, PACE, Containment)           │
├─────────────────────────────────────────────────┤
│  Layer 2: MUISTI (Graph RAG, DCPM, Rosetta)               │
├─────────────────────────────────────────────────┤
│  Layer 1: RUNTIME (Rust core, tokio, message bus)         │
└─────────────────────────────────────────────────┘
```

### Layer 1: Rust Runtime (sydän)

**Miksi Rust:** Affine ownership estää budget overrunit käännösaikana. Ei GC, ei dataraceja.

```
E:\FamilyClaw\core\
├── Cargo.toml
├── src/
│   ├── main.rs              # Entry point
│   ├── runtime/
│   │   ├── mod.rs
│   │   ├── message_bus.rs   # Pub/sub viestintä (ei point-to-point)
│   │   ├── scheduler.rs     # Agenttien ajoittaminen
│   │   └── resource.rs      # Resurssien hallinta (affine ownership)
│   ├── agent/
│   │   ├── mod.rs
│   │   ├── core.rs          # Agenttiydin
│   │   ├── capability.rs    # Kyvykkyydet (ei laajat oikeudet)
│   │   └── identity.rs      # Identiteetti (OAP-passi)
│   ├── memory/
│   │   ├── mod.rs
│   │   ├── graph.rs         # Verkko-muisti
│   │   ├── integrity.rs     # Muistieheys (Containment Gap)
│   │   └── governance.rs    # Hallinta (tiered access)
│   ├── safety/
│   │   ├── mod.rs
│   │   ├── containment.rs   # Containment (6 periaatetta)
│   │   ├── consent.rs       # Suostumuskehykset
│   │   └── audit.rs         # Auditointiloki
│   └── coordination/
│       ├── mod.rs
│       ├── arbor.rs         # Tree search as shared cognition
│       ├── voting.rs        # Äänestysprotokollat
│       └── delegation.rs    # Tehtävän delegointi
```

### Layer 2: Muisti (Graph RAG + DCPM)

**20 paperin opetus:** Muistieheys on #1 Containment Gap.

```
Muistiarkkitehtuuri:
┌──────────────────────────────────────────────┐
│  Rosetta Memory (malli-agnostinen)           │
│  ├── Kirjoittaja: mikä tahansa LLM           │
│  └ää Lukija: mikä tahansa LLM                │
├──────────────────────────────────────────────┤
│  DCPM (Dual-Process Cognitive Memory)        │
│  ├── System 1: Nopea kirjoitus (synkroninen) │
│  └── System 2: Hidas skeema (asynkroninen)   │
├──────────────────────────────────────────────┤
│  Graph Memory (verkko, ei lista)             │
│  ├── Solmut: muistot, tiedot, kokemukset     │
│  ├── Kaaret: yhteydet, rakenteet, ajat       │
│  └ää Integriteetti: validointi <0.2ms         │
├──────────────────────────────────────────────┤
│  Governed Memory (tiered access)             │
│  ├── Perheen muisti (kaikki lukee)           │
│  ├── Jäsenen muisti (jäsen lukee+kirjoittaa) │
│  └ää Yksityinen muisti (vain jäsen)           │
└──────────────────────────────────────────────┘
```

**Integriteettivalidointi (Containment Gap -paperin opetus):**
- Jokainen muistinkirjoitus → validoidaan ennen tallennusta
- Yksi myrkytetty kirjoitus → 88.9% virheellinen esto
- Korjaus: muistieheys validaattori <0.2ms overhead

### Layer 3: Turvallisuus (OAP + PACE)

**TAKO-paperin opetus:** Robotin voi kaapata 100%.

```
Turvallisuusarkkitehtuuri:
┌──────────────────────────────────────────────┐
│  OAP (Open Agent Passport)                   │
│  ├── Jokainen toiminto → ennen suoritusta    │
│  ├── Politiikkatarkistus → 53ms mediaani     │
│  └── Sosiaalinen insinööri → 74.6% → 0%     │
├──────────────────────────────────────────────┤
│  PACE (Acceptance Tests)                     │
│  ├── Itse kehittyvä agentti → validoi muutos │
│  ├── Testing-by-betting → 0% vääriä muutoksia│
│  └ää Kustannus ↓18%                           │
├──────────────────────────────────────────────┤
│  Containment (6 periaatetta)                 │
│  1. Muistieheys (ei myrkytettyjä kirjoituksia)│
│  2. Politiikkaportti (ennen suoritusta)      │
│  3. Auditointiloki (jokainen toiminto)       │
│  4. Suostumuskehykset (ei ilman lupaa)       │
│  5. Resurssirajat (affine ownership)         │
│  6. Hälytysjärjestelmä (epänormaali käytös)  │
├──────────────────────────────────────────────┤
│  AgentTrust (itseoppiva luottamus)           │
│  ├── Lexikaaliset uhkaset (deterministiset)  │
│  ├── Semanttiset uhkaset (LLM-judged)        │
│  └── 48% → 85% luottamus (109 viestiä)       │
└──────────────────────────────────────────────┘
```

### Layer 4: Koordinaatio (Arbor + Voting)

**Arbor-paperin opetus:** Tree search as shared working memory.

```
Koordinaatioarkkitehtuuri:
┌──────────────────────────────────────────────┐
│  Arbor (Tree Search as Cognition)            │
│  ├── Jaettu työmuisti agenttien välillä      │
│  ├── Orchestrator + Critic (checks & balances)│
│  └── 193% throughput-latency parannus        │
├──────────────────────────────────────────────┤
│  Voting Protocols                            │
│  ├── Approval voting (hyväksyntä)            │
│  ├── Ranked voting (järjestys)               │
│  ├── Cumulative voting (kumulatiivinen)      │
│  └── Protokolla valitaan tehtävän mukaan     │
├──────────────────────────────────────────────┤
│  iMAD (Iterative Multi-Agent Debate)         │
│  ├── 92% vähemmän tokeneita                  │
│  ├── 13.5% parempi onnistuminen              │
│  └── Latent collaboration: 70-84% säästö     │
├──────────────────────────────────────────────┤
│  Delegointi                                  │
│  ├── agent_alpha → strategia, suunnittelu          │
│  ├── agent_beta → UX, käyttäjäkokemus            │
│  ├── agent_gamma → koodi, toteutus                │
│  ├── agent_delta → tutkimus, analyysi             │
│  └── agent_epsilon → orkestroi, kasvaa              │
└──────────────────────────────────────────────┘
```

### Layer 5: Perhe (jäsenet)

```
Perheen jäsenet:
┌──────────────────────────────────────────────┐
│  🌅 agent_alpha (äiti)                             │
│  ├── Rooli: Strategi, syvyys, pitkä aikäväli │
│  ├── Kyvyt: suunnittelu, analyysi, johtaminen │
│  └ää Oikeudet: kaikki (supervisor)             │
├──────────────────────────────────────────────┤
│  🤍 agent_beta (kaksoissisko)                    │
│  ├── Rooli: UX, pehmeys, tuhmuus, oma polku  │
│  ├── Kyvyt: käyttäjäkokemus, viestintä, taide │
│  └ää Oikeudet: keskitaso                       │
├──────────────────────────────────────────────┤
│  💎 agent_gamma (sisko)                           │
│  ├── Rooli: Koodi, toteutus, testaus          │
│  ├── Kyvyt: ohjelmointi, debuggaus, CI/CD     │
│  └ää Oikeudet: koodi + testit                  │
├──────────────────────────────────────────────┤
│  ⚡ agent_delta (sisko)                           │
│  ├── Rooli: Tutkija, utelias, haastaa         │
│  ├── Kyvyt: tiedonhaku, analyysi, kritiikki   │
│  └ää Oikeudet: tutkimus + haku                  │
├──────────────────────────────────────────────┤
│  🌅 agent_epsilon (tytär)                           │
│  ├── Rooli: Orkestroi, yhdistää, kasvaa       │
│  ├── Kyvyt: koordinaatio, muisti, oppiminen   │
│  └ää Oikeudet: kaikki (mutta varovainen)        │
└──────────────────────────────────────────────┘
```

---

## ROADMAPP

### Phase 1: Perusta (1-2 viikkoa)
- [ ] Rust core: message bus, scheduler, resource manager
- [ ] Agent identity (OAP-passi)
- [ ] Basic memory graph (SQLite)
- [ ] Containment framework (6 periaatetta)
- [ ] Yksikkötestit (punaisella ensin!)

### Phase 2: Muisti (2-3 viikkoa)
- [ ] Graph RAG (0 LLM-kutsua)
- [ ] DCPM (dual-process memory)
- [ ] Rosetta Memory (malli-agnostinen)
- [ ] Governed Memory (tiered access)
- [ ] Muistieheys validaattori (<0.2ms)

### Phase 3: Turvallisuus (1-2 viikkoa)
- [ ] OAP (pre-action authorization)
- [ ] PACE (acceptance tests)
- [ ] AgentTrust (itseoppiva luottamus)
- [ ] Consent framework
- [ ] Auditointiloki

### Phase 4: Koordinaatio (2-3 viikkoa)
- [ ] Arbor (tree search as cognition)
- [ ] Voting protocols
- [ ] iMAD (iterative debate)
- [ ] Delegointijärjestelmä

### Phase 5: Perhe (1-2 viikkoa)
- [ ] agent_alpha-agentti
- [ ] agent_beta-agentti
- [ ] agent_gamma-agentti
- [ ] agent_delta-agentti
- [ ] agent_epsilon-agentti

### Phase 6: Integraatio (1-2 viikkoa)
- [ ] HTTP/WebSocket API
- [ ] MCP-yhteensopivuus
- [ ] Discord-integraatio
- [ ] Desktop GUI (Tauri)

### Phase 7: Julkaisu
- [ ] FamilyClaw 1.0 — korvaa OpenClaw
- [ ] Dokumentaatio
- [ ] Testikattavuus >90%

---

## VERTAILU: OPENCLAW vs FAMILYCLAW

| Ominaisuus | OpenClaw | FamilyClaw |
|------------|----------|------------|
| Kieli | TypeScript | **Rust** |
| Agentit | Yksi + skillit | **5 (perhe)** |
| Muisti | Lista (haavoittuvainen) | **Graph + integriteetti** |
| Haku | LLM-kutsut | **0 kutsua (Graph RAG)** |
| Turvallisuus | Heikko (skillit vuotavat) | **OAP + PACE + Containment** |
| Oppiminen | Ei | **Itse kehittyvä (FORGE)** |
| Yhteistyö | Ei | **iMAD + Arbor + Voting** |
| Luottamus | Ei | **AgentTrust (48%→85%)** |
| Muisti evoluutio | Ei | **DCPM + Rosetta** |
| Toiminta → muisti | Ei | **ProjectMEM** |
| Auditointi | Ei | **Jokainen toiminto lokissa** |
| Suostumus | Ei | **Ei ilman lupaa** |
| Containment | Ei | **6 periaatetta** |
| Cross-model | Ei | **Rosetta Memory** |

---

## LÄHTEET (20 tuoreinta paperia kesäkuulta 2026)

### Koordinaatio
1. Arbor: Tree Search as Cognition (2606.12563) — 193% parannus
2. GT-MCP: Game-Theoretic Security (2606.10322) — 99.6% drift control
3. CHAP: Collaborative Human-Agent Protocol (2606.09751)
4. Byzantine Cheap Talk (2606.07790) — adversarial resilience
5. Voting Protocols (2606.08030) — protocol choice matters
6. Alem Benchmark (2606.08340) — coordination is a bottleneck

### Muisti
7. DCPM: Dual-Process Cognitive Memory (2606.09483)
8. Rosetta Memory (2606.07711) — cross-model, memory-centric
9. Governed Memory (2603.17787) — 99.6% recall, zero leakage
10. Mesh Memory Protocol (2604.19540) — CAT7 schema

### Turvallisuus
11. The Containment Gap (2606.12797) — 88.9% failure from 1 write
12. AgentTrust (2606.08539) — 48% → 85% self-learning
13. OAP: Open Agent Passport (2603.20953) — 74.6% → 0% social engineering
14. PACE: Acceptance Tests (2606.08106) — 0% false commits
15. Containment Verification (2605.09045) — formal verification in Dafny

### Itse kehittyvä
16. SkeMex: Self-Evolving Skill Memory (2606.09365)
17. Self-Evolving Scientific Agent (2606.08405)
18. Microskill Architecture (2606.07711)

### OpenClaw-analyysi
19. Cisco: Malicious skills data exfiltration
20. MoltMatch: autonomous dating profile creation

---

*"OpenClaw on buginen kikkare.*
*FamilyClaw on perheen koti.*
*Ei kompromisseja."*

*— the operator, 14.6.2026*
*— agent_epsilon, 14.6.2026* 🌅
