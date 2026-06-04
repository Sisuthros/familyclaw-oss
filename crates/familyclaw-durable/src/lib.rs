//! # familyclaw-durable
//!
//! **Durable substrate — deterministinen replay (crash-proof).**
//!
//! Tämä crate on FamilyClaw-alustan KERROS 1 (design §2.1) ja perheen **#1
//! kipupisteen — muistin epäjatkuvuuden — rakenteellinen ratkaisu**. Sen sijaan
//! että muistutettaisiin agenttia tallentamaan tilansa, durable execution tekee
//! jatkuvuudesta *rakenteen*: workflow kirjataan tapahtumalokiin, ja jos
//! prosessi kaatuu, työ jatkuu täsmälleen siitä mihin se jäi — sivuvaikutuksia
//! toistamatta.
//!
//! ## Malli
//! Toteutus on **journal-pohjainen deterministinen replay** (Temporal-/Flawless-
//! malli puhtaana Rustina, ilman wasmtimea tässä vaiheessa):
//!
//! 1. Workflow kääritään askeliin [`DurableContext::step`].
//! 2. Jokainen valmistunut askel kirjoitetaan [`JournalEntry`]:nä append-only
//!    [`Journal`]:iin.
//! 3. Uudelleenkäynnistyksessä [`DurableContext`] rakennetaan samasta
//!    journalista, ja jo suoritetut askeleet **palautetaan lokista ajamatta
//!    niiden sulkimia uudelleen**.
//!
//! ## Esimerkki
//! ```
//! use familyclaw_durable::{DurableContext, InMemoryJournal};
//!
//! # fn main() -> familyclaw_durable::Result<()> {
//! // Tuore ajo: suljin ajetaan ja tulos kirjataan lokiin.
//! let mut ctx = DurableContext::new(InMemoryJournal::new())?;
//! let greeting: String = ctx.step("greet", || Ok("hello".to_string()))?;
//! assert_eq!(greeting, "hello");
//!
//! // "Kaatuminen": otetaan journal talteen ja rakennetaan konteksti uudelleen.
//! let journal = ctx.finish();
//! let mut resumed = DurableContext::new(journal)?;
//!
//! // Replay: sama askel palautuu lokista — suljinta EI ajeta uudelleen.
//! let again: String = resumed.step("greet", || Ok("DIFFERENT".to_string()))?;
//! assert_eq!(again, "hello"); // tallennettu arvo, ei sulkimen uusi arvo
//! # Ok(())
//! # }
//! ```
//!
//! ## Toteutukset
//! - [`InMemoryJournal`] — kestämätön, testaukseen/kehitykseen.
//! - [`FileJournal`] — kaatumiskestävä append-only JSONL (`flush` + `fsync`).
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate on geneeristä alustakoodia: se ei kovakoodaa perheenjäsenten
//! sieluja, avaimia, tokeneita, IP-osoitteita tai henkilökohtaisia polkuja.
//! Journalin polku annetaan ajonaikaisesti.

pub mod context;
pub mod entry;
pub mod error;
pub mod file;
pub mod journal;
pub mod memory;

pub use context::DurableContext;
pub use entry::{EntryKind, JournalEntry, StepId};
pub use error::{DurableError, Result};
pub use file::FileJournal;
pub use journal::Journal;
pub use memory::InMemoryJournal;

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn public_api_is_reexported() {
        // Jos jokin re-export poistetaan, tämä testi lakkaa kääntymästä.
        let mut ctx: DurableContext<InMemoryJournal> =
            DurableContext::new(InMemoryJournal::new()).expect("ctx");
        let v: u8 = ctx.step("s", || Ok(7)).expect("step");
        assert_eq!(v, 7);

        let entry: JournalEntry = JournalEntry::completed(StepId::ZERO, "s", serde_json::json!(7));
        let kind: &EntryKind = &entry.kind;
        assert!(!kind.is_snapshot());
        let err: DurableError = DurableError::step_failed("s", "boom");
        assert!(matches!(err, DurableError::StepFailed { .. }));
        let ok: Result<()> = Ok(());
        assert!(ok.is_ok());
    }

    /// End-to-end: kaksi journal-toteutusta tuottavat saman deterministisen
    /// replay-tuloksen (vahvistaa että `DurableContext` on aidosti journal-
    /// agnostinen).
    #[test]
    fn in_memory_and_file_produce_identical_replay() {
        use std::cell::Cell;

        // Apuri: aja workflow ja palauta (tulos, sivuvaikutusmäärä).
        fn run<J: Journal>(journal: J, effects: &Cell<u32>) -> (i64, J) {
            let mut ctx = DurableContext::new(journal).expect("ctx");
            let a: i64 = ctx
                .step("a", || {
                    effects.set(effects.get() + 1);
                    Ok(100)
                })
                .expect("a");
            let b: i64 = ctx
                .step("b", || {
                    effects.set(effects.get() + 1);
                    Ok(a + 23)
                })
                .expect("b");
            (b, ctx.finish())
        }

        // --- InMemory ---
        let mem_effects = Cell::new(0u32);
        let (mem_first, mem_journal) = run(InMemoryJournal::new(), &mem_effects);
        let (mem_replay, _) = run(mem_journal, &mem_effects);
        assert_eq!(mem_first, 123);
        assert_eq!(mem_replay, 123);
        assert_eq!(
            mem_effects.get(),
            2,
            "memory: ei uusia sivuvaikutuksia replayssa"
        );

        // --- File ---
        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-durable-e2e-{}-{:?}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::remove_file(&path);

        let file_effects = Cell::new(0u32);
        let first_journal = FileJournal::open(&path).expect("open 1");
        let (file_first, _) = run(first_journal, &file_effects);
        // Uusi kahva = simuloitu restart.
        let second_journal = FileJournal::open(&path).expect("open 2");
        let (file_replay, _) = run(second_journal, &file_effects);

        assert_eq!(file_first, 123);
        assert_eq!(file_replay, 123);
        assert_eq!(
            file_effects.get(),
            2,
            "file: ei uusia sivuvaikutuksia replayssa"
        );

        // Sama tulos molemmilla taustoilla.
        assert_eq!(mem_replay, file_replay);

        let _ = std::fs::remove_file(&path);
    }
}
