//! [`PostgresJournal`] — append-only journal on `PostgreSQL` (feature `postgres`).
//!
//! Same [`Journal`](crate::Journal) contract as [`crate::FileJournal`]: sync
//! append with durable commit before return. Still a **single-tenant**
//! appliance backend — not a multi-node consensus log.
//!
//! Schema (created on open):
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS familyclaw_journal (
//!   seq BIGSERIAL PRIMARY KEY,
//!   payload JSONB NOT NULL,
//!   created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
//! );
//! ```

use std::sync::Mutex;

use postgres::{Client, NoTls};

use crate::entry::{JournalEntry, StepId};
use crate::error::{DurableError, Result};
use crate::journal::Journal;

/// Environment variable for the Postgres connection URL.
pub const DATABASE_URL_ENV: &str = "DATABASE_URL";

/// Crash-resistant journal backed by `PostgreSQL`.
pub struct PostgresJournal {
    client: Mutex<Client>,
}

impl PostgresJournal {
    /// Connects using `DATABASE_URL` and ensures the schema exists.
    ///
    /// # Errors
    /// [`DurableError::Io`] when the URL is missing or connect fails.
    pub fn from_env() -> Result<Self> {
        let url = std::env::var(DATABASE_URL_ENV).map_err(|_| {
            DurableError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("{DATABASE_URL_ENV} must be set for PostgresJournal"),
            ))
        })?;
        Self::connect(&url)
    }

    /// Connects to `database_url` and ensures the schema exists.
    pub fn connect(database_url: &str) -> Result<Self> {
        let mut client = Client::connect(database_url, NoTls).map_err(|e| {
            DurableError::Io(std::io::Error::other(format!("postgres connect: {e}")))
        })?;
        client
            .batch_execute(
                "CREATE TABLE IF NOT EXISTS familyclaw_journal (
                    seq BIGSERIAL PRIMARY KEY,
                    payload JSONB NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );",
            )
            .map_err(|e| {
                DurableError::Io(std::io::Error::other(format!("postgres schema: {e}")))
            })?;
        Ok(Self {
            client: Mutex::new(client),
        })
    }
}

impl Journal for PostgresJournal {
    fn append(&self, entry: JournalEntry) -> Result<()> {
        let json = serde_json::to_value(&entry)?;
        let mut client = self
            .client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        client
            .execute(
                "INSERT INTO familyclaw_journal (payload) VALUES ($1)",
                &[&json],
            )
            .map_err(|e| {
                DurableError::Io(std::io::Error::other(format!("postgres append: {e}")))
            })?;
        Ok(())
    }

    fn replay_from(&self, from: StepId) -> Result<Vec<JournalEntry>> {
        let mut client = self
            .client
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let rows = client
            .query(
                "SELECT payload FROM familyclaw_journal ORDER BY seq ASC",
                &[],
            )
            .map_err(|e| {
                DurableError::Io(std::io::Error::other(format!("postgres replay: {e}")))
            })?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let value: serde_json::Value = row.get(0);
            let entry: JournalEntry = serde_json::from_value(value)?;
            out.push(entry);
        }
        Ok(filter_from(out, from))
    }
}

/// Filters `entries` to only those at or after `from`.
///
/// Split out from [`PostgresJournal::replay_from`] so this part of the
/// contract -- everything except the actual SQL round-trip -- has a unit
/// test that does not require a live Postgres connection.
fn filter_from(entries: Vec<JournalEntry>, from: StepId) -> Vec<JournalEntry> {
    entries
        .into_iter()
        .filter(|entry| entry.step_id.index() >= from.index())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_errors_when_unset() {
        // Avoid mutating process env (`unsafe-code` forbid). Skip when set.
        if std::env::var(DATABASE_URL_ENV).is_ok() {
            eprintln!("skipping from_env unset check — {DATABASE_URL_ENV} already set");
            return;
        }
        match PostgresJournal::from_env() {
            Err(e) => assert!(e.to_string().contains(DATABASE_URL_ENV)),
            Ok(_) => panic!("expected Err when {DATABASE_URL_ENV} unset"),
        }
    }

    #[test]
    fn connect_rejects_garbage_url() {
        match PostgresJournal::connect("postgres://127.0.0.1:1/nope") {
            Err(e) => assert!(e.to_string().contains("postgres")),
            Ok(_) => panic!("expected connect failure"),
        }
    }

    #[test]
    fn filter_from_keeps_entries_at_or_after_index() {
        let e0 = JournalEntry::completed(StepId::new(0), "a", serde_json::json!({"v": 0}));
        let e1 = JournalEntry::completed(StepId::new(1), "b", serde_json::json!({"v": 1}));
        let e2 = JournalEntry::completed(StepId::new(2), "c", serde_json::json!({"v": 2}));
        let filtered = filter_from(vec![e0.clone(), e1.clone(), e2.clone()], StepId::new(1));
        assert_eq!(filtered, vec![e1, e2]);
    }

    #[test]
    fn filter_from_empty_when_all_before_index() {
        let e0 = JournalEntry::completed(StepId::new(0), "a", serde_json::json!({}));
        let filtered = filter_from(vec![e0], StepId::new(5));
        assert!(filtered.is_empty());
    }

    /// Integration: set `DATABASE_URL` to a real Postgres to exercise round-trip.
    #[test]
    fn roundtrip_when_database_url_set() {
        let Ok(url) = std::env::var(DATABASE_URL_ENV) else {
            eprintln!("skipping postgres roundtrip -- {DATABASE_URL_ENV} unset");
            return;
        };
        if url.trim().is_empty() {
            return;
        }
        let journal = PostgresJournal::connect(&url).expect("connect");
        let step = StepId::new(42);
        let entry = JournalEntry::completed(step, "pg_probe", serde_json::json!({"k": "v"}));
        journal.append(entry).expect("append");
        let rows = journal.replay_from(step).expect("replay");
        assert!(rows.iter().any(|e| e.step_id == step));
    }
}
