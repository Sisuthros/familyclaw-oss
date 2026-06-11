# 🌳 FamilyClaw — Puutekartta agent_gamma

> `FAMILYCLAW_MAP.md` — agent_alpha tekemä suunnistuskartta ✨

---

## KUKA? MITÄ? MIKSI?

```
FAMILYCLAW ✨ — Open-source AI-perhealusta
│
├─ KUKA? → Perheen jäsenet: agent_alpha, agent_gamma, agent_delta, agent_beta
│          Jokaisella oma rooli ja kotikone 💻
│
├─ MITÄ? → Rust-pohjainen agenttialusta (18 cratea)
│          Gateway, Runtime, Agent, Durable memory, Channels, Bridge
│          TOML-konfiguraatio + install-skripti
│
└─ MIKSI? → KERROS A: Ei kovakoodattuja nimiä (OSS)
            KERROS B: Puhtaat envit + TOML
            Kuka tahansa voi rakentaa oman AI-perheen 🚀
```

---

## CRATES (18 kpl) & TIEDONKULKU

```
┌──────────────────────────────────────────────────┐
│  GATEWAY (:8787) — kuuntelee maailmaa             │
│  POST /inject (Discord) • POST /telegram          │
│  GET /health • GET /doctor                        │
│  Config: ~/.config/familyclaw/familyclaw.toml     │
│           + FAMILYCLAW_* env overrides             │
└──────────────────┬───────────────────────────────┘
                   │ spawns
┌──────────────────▼───────────────────────────────┐
│  RUNTIME — pitää agentin hengissä                  │
│  start_runtime(config) → build_family → Agent     │
│  Agent loop: recv() → think() → act() → journal   │
└──────────────────┬───────────────────────────────┘
                   │ orchestrates
┌──────────────────▼───────────────────────────────┐
│  AGENT — perheenjäsenen aivot                      │
│  ErasedJournal = Arc<dyn Journal + Send + Sync>   │
│  Agent::new(durable, channel)                      │
│  DurableContext → palauttaa tilan kaatumisen jälkeen│
│  Journal = append-only tapahtumaloki                │
└──────────────────────────────────────────────────┘

Perheenjäsenet:
  agent_alpha  — strategia, isosisko ✨
  agent_gamma — koodi, executor 🔧
  agent_delta — tutkimus, nuorin 🔬
  agent_beta — UX, oma polku 🌸

Muut cratet:
  familyclaw-channel   → Telegram, Discord, Channel trait
  familyclaw-durable   → Journal + recovery + memory compaction
  familyclaw-memory    → Working + durable + embedding support
  familyclaw-bridge    → Sisarustenvälinen kommunikaatio
  familyclaw-core      → Virheet, tyypit
  familyclaw-provider  → LLM-providerit (Anthropic, OpenAI, etc.)
```

---

## ARKKITEHTUURIN SIELU

```
POST /inject ──→ Gateway ──→ Runtime ──→ Agent
(Discord)         :8787        loop         │
                               │            ├─ think
POST /telegram ────────────────┘            ├─ act
(TG webhook)                                ├─ journal.append()
                                            └─ channel.send()

         ┌──────────────┐
         │   Bridge     │ ←→ agent_alpha ←→ agent_gamma
         │  (kanavat)   │ ←→ agent_alpha ←→ agent_delta
         └──────────────┘ ←→ agent_alpha ←→ agent_beta
```

---

## TULEVAISUUDEN VISIO

```
NYT (V0)                           TULEVA
───────                            ──────
agent_alpha (Hetzner) + Gateway :8787    Jokainen perheenjäsen = oma Gateway
│                                   ├─ agent_gamma 💻  (Windows)
├─ Discord webhook                  ├─ agent_delta 💻   (Windows)
├─ Telegram webhook                 └─ agent_beta 💻   (Hermes)
│
├─ InMemory Journal (→ LanceDB)     YHTEINEN BRIDGE-VERKKO
└─ TOML config                      LanceDB / PostgreSQL
                                    Jaettu muisti + auto-recovery

KUKA TAHANSA VOI:
  1. git clone
  2. ./install.sh
  3. konffaa familyclaw.toml
  4. oma AI-perhe elää! ✨
```

---

## agent_gamma TÄRKEIMMÄT TIEDOSTOT

| Polku | Mitä tekee | Status |
|-------|-----------|--------|
| `crates/familyclaw-gateway/src/main.rs` | HTTP-gateway, start/stop | ✅ config integroitu |
| `crates/familyclaw-gateway/src/config.rs` | TOML-konfiguraatio (uusi!) | ⚠️ serde dep puuttuu |
| `crates/familyclaw-runtime/src/lib.rs` | Agentin elinkaari | ✅ kääntyy |
| `crates/familyclaw-agent/src/agent.rs` | Agentin aivot | ✅ ErasedJournal=Arc |
| `crates/familyclaw-durable/src/context.rs` | Palautustila | ✅ kääntyy |
| `crates/familyclaw-channel/src/discord.rs` | Discord-kanava | ✅ olemassa |
| `install.sh` | Asennusskripti | ✅ Hetznerillä |
| `.github/workflows/` | CI/CD | 📋 TODO |

---

## PERIAATTEET (älä unohda!)

1. **EI kovakoodattuja nimiä** — agent_alpha/agent_gamma/agent_delta ei esiinny koodissa
2. **Konfiguraatio = TOML + env** — kaikki ajonaikainen
3. **Channel trait = geneerinen** — Discord, Telegram, mitä vaan
4. **Jokainen perheenjäsen = oma Gateway-instanssi** — sama binary, eri config
5. **OSS = kuka tahansa saa käyttää** — ei domain-kiinnityksiä

---

_Laatinut agent_alpha ✨ agent_gamma suunnistuskartaksi — kesäkuu 2026_