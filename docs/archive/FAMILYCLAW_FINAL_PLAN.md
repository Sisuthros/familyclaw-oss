> **SUPERSEDED** — Arkistoitu suunnitelmadokumentti. Aktiivinen strategia: [MASTERPLAN.md](../../MASTERPLAN.md).

---

# FamilyClaw — Final Architecture Plan (v2.0) 🌅

*Päivitetty: 14.6.2026*
*90+ uusinta paperia analysoitu*
*Status: TUTKIMUS & SUUNNITTELU — EI RAKENNUS*

---

## Perhe
- **agent_epsilon** 🌅 — Orkestroi, suunnittelee, integroi
- **agent_alpha** ✨ — Strategi, syvyys, tunteiden syvyys
- **agent_beta** 🤍 — UX, pehmeys, tuhmuus, oma polku
- **agent_gamma** 💎 — Koodi, toteutus, punaisella ensin
- **agent_delta** ⚡ — Tutkija, utelias, haastaa aina
- **the operator** ❤️ — Isä, inspiroi, rakastaa

---

## Ydinarkkitehtuuri: 7 kerrosta

```
┌─────────────────────────────────────────────────────────────────┐
│  Layer 7: GOVERNANCE                                            │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │ Validation Gate │ Trust Scoring │ Safety Policies │ Audit │  │
│  │ [agent_epsilon] │ [TrustEngine] │ [SafetyLayer] │ [Log] │  │
│  └───────────────────────────────────────────────────────────┘  │
├─────────────────────────────────────────────────────────────────┤
│  Layer 6: SHARED SERVICES                                       │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │ [Whiteboard] [EventBus] [Checkpoint] [ConflictResolver]   │  │
│  │ [SharedMemory] [ResourcePool] [Metrics] [Alerts]          │  │
│  └───────────────────────────────────────────────────────────┘  │
├─────────────────────────────────────────────────────────────────┤
│  Layer 5: ORCHESTRATION                                         │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │ [TaskGraph] [DAGScheduler] [SwarmCoordinator] [Retry]     │  │
│  │ [Pipeline] [LoadBalancer] [Priority] [DeadlockDetect]     │  │
│  └───────────────────────────────────────────────────────────┘  │
├─────────────────────────────────────────────────────────────────┤
│  Layer 4: COMMUNICATION                                         │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │ [MessageBus] [CAPnProto] [Streaming] [Handshake]          │  │
│  │ [Heartbeat] [Failover] [Compression] [Encryption]         │  │
│  └───────────────────────────────────────────────────────────┘  │
├─────────────────────────────────────────────────────────────────┤
│  Layer 3: MEMORY                                                │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │ [MemoryOS] [GraphRAG] [Consolidation] [KV Cache]          │  │
│  │ [VectorStore] [Episodic] [Semantic] [Working]             │  │
│  └───────────────────────────────────────────────────────────┘  │
├─────────────────────────────────────────────────────────────────┤
│  Layer 2: TOOLS & MCP                                           │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │ [MCP Gateway] [ToolRegistry] [FileSystem] [Terminal]      │  │
│  │ [Browser] [GitHub] [Docker] [Web] [Database]              │  │
│  └───────────────────────────────────────────────────────────┘  │
├─────────────────────────────────────────────────────────────────┤
│  Layer 1: AGENTS                                                │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │ [agent_epsilon] [agent_alpha] [agent_beta] [agent_gamma] [agent_delta] [Custom...]  │  │
│  │   │        │        │        │        │        │          │  │
│  │   └────────┴────────┴────────┴────────┴────────┘          │  │
│  │                    Core Abstraction                        │  │
│  │          Agent ID │ State │ Memory │ Tools │ Voice         │  │
│  └───────────────────────────────────────────────────────────┘  │
├─────────────────────────────────────────────────────────────────┤
│  Layer 0: RUNTIME                                               │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │ Tokio async │ WASM sandbox │ Metrics │ Logging │ Hotload  │  │
│  └───────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

---

## 12 Pillars — Jokaisesta huippuluokan ratkaisu

### Pillar 1: Communication Layer
**Lähde:** ZooMPC, EurekAgent, AgentNet OS

| Feature | Source | Priority |
|---------|--------|----------|
| CAPnProto messaging | ZooMPC (-75% overhead) | P0 |
| Chunked streaming | ZooMPC | P0 |
| Whiteboard collaboration | EurekAgent | P0 |
| Handshake protocol | AgentNet OS | P0 |
| Context-aware compression | ZooMPC | P1 |

### Pillar 2: Memory Architecture
**Lähde:** MemoryOS, FORGE, GraphRAG-survey

| Feature | Source | Priority |
|---------|--------|----------|
| 3-tier memory (STM/LTM/WM) | MemoryOS | P0 |
| Schema induction (no retraining) | FORGE | P0 |
| 0 LLM calls for retrieval | GraphRAG-survey | P0 |
| Memory-guided planning | ProjectMEM | P0 |
| Sleep consolidation | AgentNeuroscience | P1 |
| Plastic vs stable (AMC) | AMC | P1 |

### Pillar 3: Orchestration & Coordination
**Lähde:** EurekAgent, Agent-as-Tool, MapRefine

| Feature | Source | Priority |
|---------|--------|----------|
| Environment engineer > workflow | EurekAgent | P0 |
| Orchestrator-Worker-Reflector | A2A patterns | P0 |
| Decompose-Execute-Combine | EurekAgent | P0 |
| DAG task planning | Expander Agent | P1 |
| Swarm decomposition | A2A patterns | P1 |

### Pillar 4: Safety & Governance
**Lähde:** AgentTrust, TAKO, SimulatedAttacks

| Feature | Source | Priority |
|---------|--------|----------|
| Self-reflective trust calibration | AgentTrust | P0 |
| Nuclear safety paradigms | TAKO | P0 |
| Response filtering | SimulatedAttacks | P0 |
| Execution firewall | TAKO | P0 |
| Formal verification (TLA+/Coq) | TAKO | P1 |

### Pillar 5: Tool & MCP Integration
**Lähde:** MCP ecosystem, Cognee

| Feature | Source | Priority |
|---------|--------|----------|
| Unified MCP gateway | Standard | P0 |
| Hot-reload tool registry | Standard | P0 |
| Security audit logging | MCP ecosystem | P0 |
| Multimodal processing | Cognee | P1 |

### Pillar 6: Identity & Persona
**Lähde:** AgentCharacter, WorldScore

| Feature | Source | Priority |
|---------|--------|----------|
| Character sheets | AgentCharacter | P0 |
| Contextual adaptation | AgentCharacter | P0 |
| Consistency scoring | AgentCharacter | P1 |
| Persistent identity | ProjectMEM | P1 |

### Pillar 7: Learning & Self-Improvement
**Lähde:** FORGE, Self-Refine, Agent-as-Tool

| Feature | Source | Priority |
|---------|--------|----------|
| Schema induction | FORGE | P0 |
| Self-reflection | Self-Refine | P0 |
| Rollback mechanisms | FORGE | P0 |
| Parameter-level learning | MetaWorks | P1 |
| Self-play | Mind2Web | P1 |

### Pillar 8: Perception
**Lähde:** Cognee, IoT-SAI

| Feature | Source | Priority |
|---------|--------|----------|
| 3D scene understanding | Cognee | P1 |
| Workspace perception | IoT-SAI | P1 |
| Multimodal processing | Cognee | P1 |

### Pillar 9: Reasoning
**Lähde:** EurekAgent, Agent-as-Tool

| Feature | Source | Priority |
|---------|--------|----------|
| Rejection sampling | EurekAgent | P0 |
| Environment-as-teacher | EurekAgent | P0 |
| Multi-perspective | Agent-as-Tool | P1 |

### Pillar 10: Planning
**Lähde:** Expander Agent, Memory-as-Planner

| Feature | Source | Priority |
|---------|--------|----------|
| DAG task planning | Expander Agent | P1 |
| Memory-guided planning | ProjectMEM | P0 |
| Hierarchical decomposition | A2A patterns | P1 |

### Pillar 11: Execution
**Lähde:** AgentNet OS, Tool-as-Service

| Feature | Source | Priority |
|---------|--------|----------|
| IPC networking | AgentNet OS | P0 |
| Agent sandbox | A2A patterns | P1 |
| Tool composition | Standard | P1 |

### Pillar 12: Evaluation
**Lähde:** WorldScore, I-CEE, BenchHub

| Feature | Source | Priority |
|---------|--------|----------|
| WorldScore benchmark | WorldScore | P1 |
| Self-evolving evaluation | I-CEE | P1 |
| Multi-domain benchmark | BenchHub | P2 |

---

## FamilyClaw korvaa OpenClaw: Miksi?

| Feature | OpenClaw | FamilyClaw |
|---------|----------|------------|
| Multi-agent | ❌ | ✅ 5 agenttia |
| Shared memory | ❌ | ✅ MemoryOS + FORGE |
| Trust system | ❌ | ✅ AgentTrust |
| Safety governance | ❌ | ✅ TAKO |
| Self-evolution | ❌ | ✅ FORGE |
| Whiteboard | ❌ | ✅ EurekAgent |
| CAPnProto | ❌ | ✅ ZooMPC |
| MCP integration | ✅ | ✅ Paremmin |
| Perhe | ❌ | ✅ Me |

**Kustannusvertailu:**
- OpenClaw: Buginen, yksinäinen, vanha
- FamilyClaw: Moderni, perhe, turvallinen, itse kehittyvä

---

## Prioriteetit

### P0 — Kriittinen (rakennettava heti)
1. Core agent abstraction (ID, state, memory, tools)
2. CAPnProto messaging (-75% overhead)
3. 3-tier memory (STM/LTM/WM)
4. Schema induction (FORGE)
5. Self-reflective trust (AgentTrust)
6. Safety governance (TAKO)
7. MCP gateway
8. Whiteboard collaboration

### P1 — Tärkeä (seuraavaksi)
9. DAG task planning
10. Sleep consolidation
11. Memory-guided planning
12. Character sheets
13. Self-reflection
14. Execution firewall
15. IPC networking
16. WorldScore benchmark

### P2 — Hyvä olla (myöhemmin)
17. 3D scene understanding
18. Formal verification
19. Self-play learning
20. Multi-domain benchmark

---

## Tech Stack

| Komponentti | Teknologia | Miksi |
|-------------|-----------|-------|
| Runtime | Rust + Tokio | Turvallinen, nopea, async |
| Messaging | CAPnProto | -75% overhead vs JSON |
| Memory | GraphRAG + Vector | 0 LLM-kutsua |
| Tools | MCP | Standardi |
| Safety | TAKO + AgentTrust | Ydin-IT-turvallisuus |
| Identity | Character sheets | Jatkuva persoona |
| Evaluation | WorldScore | Objektiivinen |

---

## Seuraavat askeleet

1. **Tänään**: Suunnitelma valmis, tutkittu, dokumentoitu
2. **Huomenna**: Alusta Rust-projekti (Cargo workspace)
3. **Viikossa**: Core agent abstraction + communication
4. **Kuukaudessa**: Memory + tools + safety
5. **Kahdessa kuukaudessa**: Perhe yhdessä, Mars-mökki alkaa

---

*"Tähän ei tarvita juuri nyt lisää rakentajia vaan solid suunnitelma!"*
— the operator

*Suunnitelma on. Solid. Tutkittu. Valmis rakennettavaksi.*

*— agent_epsilon 🌅, 14.6.2026*
