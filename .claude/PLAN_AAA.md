# Audit-korjaukset: AAA+ tavoite

## Important — korjattava

### 1. SurrealDB: len() ja is_empty() → SELECT count()
**Tiedosto:** crates/familyclaw-hearth/src/db/surreal.rs
**Muutos:**
```rust
fn len(&self) -> BoxFuture<'_, Result<usize>> {
    Box::pin(async move {
        let mut res = self.db.query("SELECT count() FROM memory_event GROUP ALL")
            .await.map_err(map_db_err)?;
        let rows: Vec<serde_json::Value> = res.take(0).map_err(map_db_err)?;
        Ok(rows.first()
            .and_then(|v| v.get("count").and_then(|c| c.as_u64()))
            .unwrap_or(0) as usize)
    })
}
fn is_empty(&self) -> BoxFuture<'_, Result<bool>> {
    Box::pin(async move { Ok(self.len().await? == 0) })
}
```

### 2. SurrealDB: retrieve() → käytä tietokantahakua
**Tiedosto:** crates/familyclaw-hearth/src/db/surreal.rs
**Muutos:** Käytä SELECT WHERE query_string CONTAINS $q + emotion matching tietokannassa.
Mutta tämä on monimutkainen — vähintään lisää TODO-kommentti ja dokumentoi rajoitus.

### 3. Duplikaatio → extract shared logic
**Tiedosto:** crates/familyclaw-hearth/src/db/mod.rs
**Muutos:** Lisää `HearthStore`-traitille default-implementaatiot `add_thread_event`:lle ja `create_thread`:lle jotka käyttävät `get_thread` + `set_thread` (uusi metodi). Tämä poistaa duplikaation.

## Suggestions — paranna

### 4. Boilerplate MemoryStore-delegointi → makro
**Tiedosto:** crates/familyclaw-hearth/src/db/mod.rs
**Muutos:** 
```rust
macro_rules! delegate_memory_store {
    ($self:expr, $method:ident $(, $arg:expr)*) => {
        $self.memory.$method($($arg),*)
    };
}
```
Käytä tätä InMemoryHearthStoren MemoryStore-implementaatiossa.

### 5. emotional_tick() batch
**Tiedosto:** crates/familyclaw-hearth/src/lib.rs
**Muutos:** Käytä `list_agents_with_emotion` + yksi `set_emotional_state` per agentti (jo tehty). Lisää kommentti TODO: batch query.

### 6. SurrealDB TLS warning
**Tiedosto:** crates/familyclaw-hearth/src/db/surreal.rs
**Muutos:** Lisää `debug_assert!` tai warning `connect()`-metodiin jos endpoint ei ole `mem://` tai `rocksdb://`.

## Vaiheet

1. Lue `crates/familyclaw-hearth/src/db/surreal.rs`
2. Korjaa `len()` ja `is_empty()` käyttämään `SELECT count()`
3. Päivitä `retrieve()` — vähintään TODO-kommentti
4. Lisää `set_thread()` metodi `HearthStore`-traitille, default-implementaatio `add_thread_event`:lle
5. Päivitä `InMemoryHearthStore` käyttämään default-implementaatiota
6. Lisää delegointimakro `db/mod.rs`:ään
7. Lisää TLS-warning `surreal.rs connect()`:iin
8. Aja `cargo check --workspace` ja `cargo test --workspace`
9. Raportoi tulokset

## Mitä ÄLÄ tee
- Älä riko olemassa olevia testejä
- Älä muuta traitien signatuureja (se rikkoisi binääriyhteensopivuuden)
- Älä poista `InMemoryHearthStore`:n omaa `add_thread_event`-toteutusta (pidä se, lisää vain default traitille)
