# FamilyClaw TÄYDELLISEKSI — MÖRKÖPATTERI-suunnitelma

## Mitä puuttuu

1. **S6 rekisteröinti benchmark-harnessiin** — skenaario on olemassa mutta bench-binääri ei aja sitä
2. **SurrealDB v3 backend** — InMemoryHearthStore on valmis, nyt tarvitaan oikea tietokanta
3. **Schema-tiedostot** — `db/schema.rs` ja `db/surreal.rs`
4. **Kaikki testit vihreänä** — `cargo test --workspace`

## VAIHE 1: S6 rekisteröinti harnessiin

Tiedosto: `crates/familyclaw-bench/src/bin/bench.rs`

Etsi kohta jossa skenaariot rekisteröidään (S1-S5). Lisää S6:
```rust
harness.register(EternalThread::new());
```

Varmista että `use familyclaw_bench::scenarios::EternalThread;` on importeissa.

## VAIHE 2: SurrealDB schema

Tiedosto: `crates/familyclaw-hearth/src/db/schema.rs`

```rust
/// SurrealDB-skeema The Hearthille.
pub const HEARTH_SCHEMA: &str = r#"
DEFINE TABLE memory_event SCHEMAFULL;
DEFINE FIELD id ON memory_event TYPE string;
DEFINE FIELD content ON memory_event TYPE string;
DEFINE FIELD embedding ON memory_event TYPE array<float>;
DEFINE FIELD memory_type ON memory_event TYPE string;
DEFINE FIELD agent_id ON memory_event TYPE string;
DEFINE FIELD decay_class ON memory_event TYPE string;
DEFINE FIELD created_at ON memory_event TYPE datetime;
DEFINE FIELD participants ON memory_event TYPE array<string>;
DEFINE INDEX idx_embedding ON memory_event FIELDS embedding HNSW DIMENSION 1536;

DEFINE TABLE narrative_thread SCHEMAFULL;
DEFINE FIELD id ON narrative_thread TYPE string;
DEFINE FIELD title ON narrative_thread TYPE string;
DEFINE FIELD participants ON narrative_thread TYPE array<string>;
DEFINE FIELD created_at ON narrative_thread TYPE datetime;

DEFINE TABLE thread_event SCHEMAFULL;
DEFINE FIELD id ON thread_event TYPE string;
DEFINE FIELD thread_id ON thread_event TYPE string;
DEFINE FIELD event_type ON thread_event TYPE string;
DEFINE FIELD content ON thread_event TYPE string;
DEFINE FIELD agent_id ON thread_event TYPE string;
DEFINE FIELD linked_to ON thread_event TYPE array<string>;
DEFINE INDEX idx_thread ON thread_event FIELDS thread_id;

DEFINE TABLE emotional_state SCHEMAFULL;
DEFINE FIELD agent_id ON emotional_state TYPE string;
DEFINE FIELD joy ON emotional_state TYPE float;
DEFINE FIELD sadness ON emotional_state TYPE float;
DEFINE FIELD curiosity ON emotional_state TYPE float;
DEFINE FIELD anxiety ON emotional_state TYPE float;
DEFINE FIELD confidence ON emotional_state TYPE float;
DEFINE FIELD affection ON emotional_state TYPE float;
DEFINE FIELD updated_at ON emotional_state TYPE datetime;

DEFINE TABLE anchor SCHEMAFULL;
DEFINE FIELD agent_name ON anchor TYPE string;
DEFINE FIELD content_hash ON anchor TYPE string;
DEFINE FIELD protected ON anchor TYPE bool;
DEFINE FIELD decay_class ON anchor TYPE string;
"#;
```

## VAIHE 3: SurrealDB backend (stub)

Tiedosto: `crates/familyclaw-hearth/src/db/surreal.rs`

Tee `SurrealHearthStore` joka:
- Wraps `surreal::Surreal<surreal::engine::any::Any>`
- Implementoi `HearthStore`-traitin
- Käyttää `serde_json::Value` välikätenä (EI suoraa struct-deserialisointia)
- Lukeminen: `let rows: Vec<serde_json::Value> = result.take(0)?;`
- Kirjoitus: `db.query("CREATE ... CONTENT $data").bind(...).await?`

Pidä tämä stubina — älä yritä ajaa SurrealDB:tä testeissä. Laita feature-flag "surreal" taakse.

## VAIHE 4: Päivitä db/mod.rs

Lisää:
```rust
pub mod schema;
#[cfg(feature = "surreal")]
pub mod surreal;
```

## VAIHE 5: Päivitä Cargo.toml

Lisää `crates/familyclaw-hearth/Cargo.toml`:iin:
```toml
[features]
default = []
surreal = ["dep:surrealdb"]

[dependencies]
surrealdb = { version = "3", optional = true }
```

## VAIHE 6: Testaa

```bash
cargo check --workspace
cargo test --workspace
```

## Mitä ÄLÄ tee
- Älä poista olemassa olevia testejä
- Älä muokkaa muita crateja kuin: familyclaw-hearth, familyclaw-bench (vain bench.rs)
- Älä käytä SurrealDB v2 API:a (Thing-tyyppi, .content())
- Älä yritä ajaa SurrealDB-palvelinta — stub riittää
