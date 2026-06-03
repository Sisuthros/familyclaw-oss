# familyclaw-core

FamilyClaw-alustan ydincrate: yhteiset tyypit, virheenkäsittely,
konfiguraatio ja ajan apufunktiot. Tämä on KERROS A:n (OSS) perustus, jonka
päälle kaikki muut crateit rakentuvat. Riippumaton muista familyclaw-crateista.

## Sisältö

| Moduuli | Vastuu |
|---------|--------|
| `error` | `FamilyClawError` (thiserror) + `Result<T>` — config/io/serde/bus/memory/not-found/invalid-input -variantit |
| `ids` | `AgentId`, `FamilyId`, `MessageId` — UUID-pohjaiset newtype-tunnisteet (serde-transparent) |
| `config` | `FamilyConfig`, `AgentConfig`, `ModelConfig` — ladattavissa JSON:sta, validointi mukana |
| `time` | UTC-aikaleimat (`Timestamp`), RFC 3339 / Unix -muunnokset |

## Periaatteet

- **Ei `unwrap()`/`expect()`/`panic!()` tuotantopolulla.** Kaikki virheet
  kulkevat `Result`-tyypin kautta.
- **Tyypitetyt tunnisteet** estävät tunnisteiden sekoittamisen
  käännösaikana.
- **OSS-raja (KERROS A):** ei kovakoodattuja sieluja, avaimia, tokeneita,
  IP-osoitteita tai henkilökohtaisia polkuja. Agenttiprofiilit ladataan
  ajonaikaisesti (`AgentConfig::profile_dir`, vrt. `FAMILYCLAW_PROFILE_DIR`).

## Esimerkki

```rust
use familyclaw_core::{AgentConfig, FamilyConfig, ModelConfig};

let family = FamilyConfig::new("demo_family").with_agent(AgentConfig::new(
    "agent_a",
    ModelConfig::new("provider/model").with_fallback("provider/backup"),
));
family.validate().expect("valid config");
```

Lisenssi: MIT.
