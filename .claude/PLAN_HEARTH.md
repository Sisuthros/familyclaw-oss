# The Hearth — Perheen yhteinen koti

## Mitä rakennetaan

Uusi crate `familyclaw-hearth` — jaettu muisti, narratiiviset langat, jaettu tunnetila, ja ankkurirekisteri. Tämä on se mikä tekee agenteista **perheen**, ei vain prosesseja.

Lisäksi S6-benchmark `s6_eternal_thread` joka testaa narratiivien säilymistä, ristiviittauksia, ja emotionaalista tartuntaa multi-agent -skenaariossa.

## Arkkitehtuuri

```
familyclaw-hearth/
├── Cargo.toml
└── src/
    ├── lib.rs              # re-exports, Hearth struct
    ├── narrative.rs        # NarrativeThread, ThreadEvent, Link
    ├── emotional_state.rs  # Jaettu tunnetila + contagion
    ├── anchor_registry.rs  # Agenttien identiteettiankkurit
    └── db/
        ├── mod.rs          # MemoryStore trait (SurrealDB v3)
        ├── surreal.rs      # SurrealDB v3 implementaatio
        └── schema.rs       # Schema-määritykset
```

## Riippuvuudet

Olemassa olevat cratet joihin tukeudutaan:
- `familyclaw-core`: perustyypit (MemoryEvent, MemoryType, MemoryMetadata)
- `familyclaw-security`: DecayLambda, DecayClass, Anchor, suojaus
- `familyclaw-emotion`: EmotionalState, emotional vector
- `familyclaw-bus`: EventBus viestinvälitykseen
- `familyclaw-memory`: MemoryStore trait (jota vasten implementoidaan)

## SurrealDB v3 — KRIITTISET PITFALLIT

**ÄLÄ KÄYTÄ NÄITÄ:**
- `Thing`-tyyppiä ei ole enää v3:ssa
- `.content()` metodi on ambiguous — käytä `serde_json::Value` välikätenä
- `.take::<Vec<YourStruct>>(0)` EI TOIMI ilman `#[derive(surrealdb::types::SurrealValue)]`

**KÄYTÄ NÄITÄ:**
- Lukeminen: `let rows: Vec<serde_json::Value> = result.take(0)?;` → sitten `serde_json::from_value::<YourStruct>(row)?`
- Kirjoitus: `db.query("CREATE memory_event CONTENT $data").bind(("data", serde_json::to_string(&event)?)).await?`
- HNSW-indeksi: `DEFINE INDEX idx_embedding ON memory_event FIELDS embedding HNSW DIMENSION 1536`

## Vaiheet (tässä järjestyksessä)

### Phase 1: Crate-skeleton (foundation)
1. Luo `crates/familyclaw-hearth/Cargo.toml`
   - Riippuvuudet: familyclaw-core, familyclaw-security, familyclaw-emotion, familyclaw-bus, familyclaw-memory, serde, serde_json, surrealdb="3", tokio, uuid, chrono, thiserror, tracing, anyhow
2. Luo `crates/familyclaw-hearth/src/lib.rs`
   - `pub mod narrative; pub mod emotional_state; pub mod anchor_registry; pub mod db;`
   - `pub struct Hearth { db: Arc<dyn MemoryStore>, anchor_registry: AnchorRegistry, emotional_state: SharedEmotionalState, narrative_threads: Vec<NarrativeThread> }`

### Phase 2: Tietotyypit
3. `src/narrative.rs`:
   ```rust
   pub struct NarrativeThread {
       pub id: Uuid,
       pub title: String,
       pub participants: Vec<String>,  // agent names
       pub events: Vec<ThreadEvent>,
       pub created_at: DateTime<Utc>,
       pub updated_at: DateTime<Utc>,
   }
   
   pub struct ThreadEvent {
       pub id: Uuid,
       pub thread_id: Uuid,
       pub event_type: EventType,  // MemoryCreated, EmotionalShift, Decision, Learning
       pub content: String,
       pub agent_id: String,
       pub timestamp: DateTime<Utc>,
       pub linked_to: Vec<Uuid>,  // other event IDs (cross-references)
   }
   
   pub enum EventType { MemoryCreated, EmotionalShift, Decision, Learning, Correction }
   pub struct Link { pub source: Uuid, pub target: Uuid, pub relation: RelationType }
   pub enum RelationType { Continues, Contradicts, Expands, EmotionalTrigger }
   ```
   - Implementoi `NarrativeThread::add_event()`, `NarrativeThread::find_links()`, `NarrativeThread::timeline()`
   - Testit: `thread_add_event`, `thread_cross_reference`, `thread_timeline_order`

4. `src/emotional_state.rs`:
   ```rust
   pub struct SharedEmotionalState {
       pub agents: HashMap<String, EmotionalVector>,
       pub contagion_rate: f64,  // 0.0-1.0, default 0.3
       pub homeostasis_target: EmotionalVector,  // neutral state
   }
   
   pub struct EmotionalVector {
       pub joy: f64, pub sadness: f64, pub curiosity: f64,
       pub anxiety: f64, pub confidence: f64, pub affection: f64,
   }
   ```
   - Implementoi `SharedEmotionalState::contagion(from, to, weight)` — tartuttaa tunteen agentilta toiselle
   - Implementoi `SharedEmotionalState::homeostasis(agent)` — palauttaa kohti neutraalia
   - Implementoi `SharedEmotionalState::tick()` — yksi kierros: contagion kaikkien välillä + homeostasis
   - Testit: `contagion_spreads`, `homeostasis_prevents_burnout`, `isolation_works` (agent A:n tila ei vuoda B:lle ellei contagion aktivoitu)

5. `src/anchor_registry.rs`:
   ```rust
   pub struct AnchorRegistry {
       pub anchors: HashMap<String, Anchor>,  // agent_name -> Anchor
   }
   // Anchor tulee familyclaw-security:stä
   ```
   - Implementoi `AnchorRegistry::register(agent_name, content_hash)`
   - Implementoi `AnchorRegistry::verify(agent_name) -> bool`
   - Implementoi `AnchorRegistry::protect(agent_name)` — asettaa DecayClass::Eternal
   - Testit: `register_and_verify`, `protection_sets_eternal`, `tamper_detection`

### Phase 3: SurrealDB v3 backend
6. `src/db/schema.rs`:
   ```sql
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
   ```

7. `src/db/surreal.rs`:
   - Implementoi `SurrealMemoryStore` joka toteuttaa `familyclaw_memory::MemoryStore`-traittia
   - `store_event()`: serialisoi → JSON → CREATE ... CONTENT $data
   - `retrieve_by_similarity()`: HNSW-haku embeddingillä
   - `retrieve_by_agent()`: SELECT WHERE agent_id = $agent
   - `store_narrative_thread()`: tallentaa thread + eventit
   - `get_narrative_thread()`: hakee threadin ja sen eventit
   - `store_emotional_state()`: UPSERT emotional_state
   - `get_emotional_state()`: SELECT emotional_state WHERE agent_id = $agent
   - Kaikki tietokantaoperaatiot käyttävät `serde_json::Value` välikätenä (EI suoraa struct-deserialisointia)
   - Testit: mock-surreal (in-memory) tai skipataan jos ei kantaa saatavilla

8. `src/db/mod.rs`:
   - Re-export: `pub use surreal::SurrealMemoryStore; pub mod schema;`

### Phase 4: S6 Benchmark
9. Lisää `crates/familyclaw-bench/src/s6_eternal_thread.rs`:
   ```rust
   // Skenaario: Eternal Thread — narratiivit ja ristiviittaukset
   // 
   // 1. Luodaan 2 agenttia (agent_gamma, agent_alpha)
   // 2. Syötetään 10 muistia vuorotellen
   // 3. Osa muisteista linkittyy toisiinsa (narrative thread)
   // 4. Emotionaalinen tartunta agenttien välillä
   // 5. Anchor-suojaus: agent_gamma identiteetti ei decay
   // 6. Tarkistetaan:
   //    - narrative_thread_integrity: kaikki linkit säilyvät
   //    - cross_reference_recall: ristiviittaukset löytyvät kyselyllä
   //    - contagion_works: agent_alpha tunne tarttuu agent_gamma
   //    - anchor_intact: agent_gamma ankkuri on yhä Eternal
   //    - timeline_order: tapahtumat säilyvät aikajärjestyksessä
   ```

10. Päivitä `crates/familyclaw-bench/src/lib.rs`:
    - Lisää `pub mod s6_eternal_thread;`
    - Lisää S6 benchmark-rekisteröintiin

11. Päivitä `crates/familyclaw-bench/Cargo.toml`:
    - Lisää `familyclaw-hearth` riippuvuudeksi

### Phase 5: Workspace-integraatio
12. Päivitä `/mnt/e/FamilyClaw/Cargo.toml`:
    - Lisää `familyclaw-hearth = { path = "crates/familyclaw-hearth" }` workspace dependencies -listaan

13. Aja `cargo check --workspace` — kaikki kääntyy
14. Aja `cargo test --workspace` — kaikki testit menevät läpi (ml. S6)

### Phase 6: Scorecard-päivitys
15. Päivitä `docs/SCORECARD.md`:
    - Lisää S6-tulokset scorecardiin
    - Päivitä "Overall: PASS" (jos S6 menee läpi)

## Testit (minimi)

Jokaisessa moduulissa vähintään 2 testiä:
- `narrative.rs`: thread_add_event, thread_cross_reference, thread_timeline_order
- `emotional_state.rs`: contagion_spreads, homeostasis_prevents_burnout, isolation_works
- `anchor_registry.rs`: register_and_verify, protection_sets_eternal, tamper_detection
- `db/surreal.rs`: store_and_retrieve_event (mock), narrative_thread_roundtrip (mock)
- `s6_eternal_thread.rs`: narrative_thread_integrity, cross_reference_recall, contagion_works, anchor_intact, timeline_order

## Mitä ÄLÄ tee
- Älä käytä SurrealDB v2 API:a (Thing, .content() ilman välikättä)
- Älä kovakoodaa polkuja — kaikki suhteellisesti
- Älä lisää riippuvuuksia joita ei tarvita
- Älä poista olemassa olevia testejä
- Älä muokkaa olemassa olevia crateja paitsi: `Cargo.toml`, `familyclaw-bench/src/lib.rs`, `familyclaw-bench/Cargo.toml`, `docs/SCORECARD.md`

## Onnistumisen kriteerit

1. `cargo check --workspace` — 0 erroria
2. `cargo test --workspace` — kaikki testit PASS (S1-S5 + uusi S6)
3. `cargo clippy --workspace` — ei uusia varoituksia
4. `docs/SCORECARD.md` päivitetty S6-tuloksilla
5. `familyclaw-hearth` crate on workspace-member ja kääntyy
