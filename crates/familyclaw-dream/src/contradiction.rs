//! Ristiriita-/vanhentumismerkinnät durable-journalin yli.
//!
//! `drop_contradicted`-vaihe (design §2.3) poistaa muistot jotka uudempi
//! tieto on tehnyt vääräksi (esim. "`agent_a` on kaupungissa A" kun myöhempi
//! tieto sanoo "`agent_a` on kaupungissa B"). FamilyClaw-arkkitehtuurissa **durable-journal
//! on totuuden lähde** (design §1: *"durable carries everything; dreaming
//! eats the durable log"*), joten unijakso ei arvaa ristiriitoja vaan lukee
//! ne journalista.
//!
//! Tämä moduuli antaa:
//! - vakiomuotoisen tavan **kirjoittaa** ristiriitamerkintä journaliin
//!   ([`mark_contradicted`]),
//! - tavan **lukea** kaikki merkityt muisto-id:t takaisin journalista
//!   ([`contradicted_ids`]).
//!
//! Konventio: merkintä on [`familyclaw_durable::JournalEntry`], jonka laji on
//! [`EntryKind::Marker`] nimellä [`CONTRADICT_STEP`] ja hyötykuormalla
//! JSON-objekti `{ "memory": "<uuid>" }`. **Marker ei ole workflow-askel**:
//! [`DurableContext`](familyclaw_durable::DurableContext) suodattaa markerit
//! replay-kursoristaan (kuten snapshotit), joten sama append-only-loki voi
//! turvallisesti kantaa sekä durable-workflowit että unijakson
//! ristiriitatiedon — ilman erillistä sivukanavaa ja ilman vaaraa että
//! merkintä näyttäytyisi askeleena ja laukaisisi `NondeterministicReplay`:n.

use std::collections::BTreeSet;

use familyclaw_core::MessageId;
use familyclaw_durable::{EntryKind, Journal, JournalEntry, StepId};

/// Markerin nimi jolla ristiriitamerkinnät tunnistetaan journalista.
pub const CONTRADICT_STEP: &str = "memory_contradicted";

/// JSON-avain joka kantaa ristiriitaisen muiston tunnisteen.
const MEMORY_KEY: &str = "memory";

/// Kirjoittaa journaliin **marker-merkinnän** että `memory` on
/// vanhentunut/ristiriitainen.
///
/// Merkintä on append-only [`EntryKind::Marker`]-rivi: se elää samassa lokissa
/// kuin durable-workflowit, mutta **ei kuluta replay-askelkursoria**, joten se
/// ei voi sekoittua workflow-askeleeseen. Rivin sekvenssipaikka johdetaan
/// journalin nykyisestä pituudesta.
///
/// # Errors
/// Palauttaa [`familyclaw_durable::DurableError`]:n jos journaliin
/// kirjoittaminen epäonnistuu.
pub fn mark_contradicted<J: Journal>(
    journal: &mut J,
    memory: MessageId,
) -> familyclaw_durable::Result<()> {
    let step = StepId::new(journal.len()? as u64);
    let entry = JournalEntry::marker(
        step,
        CONTRADICT_STEP,
        serde_json::json!({ MEMORY_KEY: memory.to_string() }),
    );
    journal.append(entry)
}

/// Poimii yhden journal-rivin ristiriitaisen muiston tunnisteen, jos rivi on
/// kelvollinen ristiriitamerkintä.
///
/// Palauttaa `None` jos rivi ei ole [`CONTRADICT_STEP`]-merkintä tai sen
/// hyötykuorma on muotoa jota ei tunnisteta (tuntematon ⇒ ohitetaan, ei
/// virhettä — vanhat/vieraat rivit eivät kaada unijaksoa).
fn id_from_entry(entry: &JournalEntry) -> Option<MessageId> {
    let EntryKind::Marker { name, payload } = &entry.kind else {
        return None;
    };
    if name != CONTRADICT_STEP {
        return None;
    }
    let raw = payload.get(MEMORY_KEY)?.as_str()?;
    raw.parse::<MessageId>().ok()
}

/// Lukee journalista kaikki ristiriitaisiksi merkityt muisto-id:t.
///
/// Tuntemattomat tai virheelliset rivit ohitetaan hiljaa (CLAUDE.md: älä
/// kaadu vieraaseen dataan). Tulos on **deduplikoitu** ja deterministisesti
/// järjestetty ([`BTreeSet`]), jotta unijakso on toistettava.
///
/// # Errors
/// Palauttaa [`familyclaw_durable::DurableError`]:n jos journalia ei voi
/// lukea.
pub fn contradicted_ids(
    journal: &(dyn Journal + Send + Sync),
) -> familyclaw_durable::Result<BTreeSet<MessageId>> {
    let entries = journal.replay_all()?;
    Ok(entries.iter().filter_map(id_from_entry).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_durable::InMemoryJournal;
    use serde_json::json;

    #[test]
    fn mark_then_read_roundtrip() {
        let mut journal = InMemoryJournal::new();
        let a = MessageId::new();
        let b = MessageId::new();
        mark_contradicted(&mut journal, a).expect("mark a");
        mark_contradicted(&mut journal, b).expect("mark b");

        let ids = contradicted_ids(&journal).expect("read");
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a));
        assert!(ids.contains(&b));
    }

    #[test]
    fn duplicates_are_deduplicated() {
        let mut journal = InMemoryJournal::new();
        let a = MessageId::new();
        mark_contradicted(&mut journal, a).expect("mark 1");
        mark_contradicted(&mut journal, a).expect("mark 2");
        let ids = contradicted_ids(&journal).expect("read");
        assert_eq!(ids.len(), 1);
        assert!(ids.contains(&a));
    }

    #[test]
    fn unrelated_entries_are_ignored() {
        let mut journal = InMemoryJournal::new();
        // Tavallinen workflow-askel, ei ristiriitamerkintä.
        journal
            .append(JournalEntry::completed(
                StepId::ZERO,
                "do_work",
                json!({"ok": true}),
            ))
            .expect("append work");
        // Snapshot.
        journal
            .append(JournalEntry::snapshot(StepId::new(1), json!({"state": 1})))
            .expect("append snapshot");
        // Oikea merkintä.
        let real = MessageId::new();
        mark_contradicted(&mut journal, real).expect("mark");

        let ids = contradicted_ids(&journal).expect("read");
        assert_eq!(ids.len(), 1);
        assert!(ids.contains(&real));
    }

    #[test]
    fn malformed_marker_payload_is_skipped() {
        let journal = InMemoryJournal::new();
        // Oikea marker-nimi mutta väärä hyötykuorma (ei "memory"-avainta).
        journal
            .append(JournalEntry::marker(
                StepId::ZERO,
                CONTRADICT_STEP,
                json!({"wrong": "shape"}),
            ))
            .expect("append");
        // Oikea marker-nimi, "memory" ei ole kelvollinen uuid.
        journal
            .append(JournalEntry::marker(
                StepId::new(1),
                CONTRADICT_STEP,
                json!({"memory": "not-a-uuid"}),
            ))
            .expect("append");

        let ids = contradicted_ids(&journal).expect("read");
        assert!(ids.is_empty());
    }

    #[test]
    fn step_named_like_marker_is_not_a_contradiction() {
        // Workflow-askel jonka nimi sattuu olemaan CONTRADICT_STEP EI ole
        // ristiriitamerkintä — vain `EntryKind::Marker` lasketaan.
        let journal = InMemoryJournal::new();
        journal
            .append(JournalEntry::completed(
                StepId::ZERO,
                CONTRADICT_STEP,
                json!({"memory": MessageId::new().to_string()}),
            ))
            .expect("append step");
        let ids = contradicted_ids(&journal).expect("read");
        assert!(ids.is_empty(), "askelta ei saa tulkita markeriksi");
    }

    #[test]
    fn empty_journal_has_no_contradictions() {
        let journal = InMemoryJournal::new();
        let ids = contradicted_ids(&journal).expect("read");
        assert!(ids.is_empty());
    }

    /// Regressio (review issue #3): kun **sama jaettu loki** kantaa sekä
    /// durable-workflowin että ristiriitamerkinnän, [`DurableContext`]:n
    /// rakentaminen lokin päälle ja workflowin uudelleenajo EI saa tulkita
    /// marker-riviä workflow-askeleeksi (ei `NondeterministicReplay`-virhettä),
    /// ja muistikirjaus-sivuvaikutukset eivät saa toistua replayssa.
    #[test]
    fn durable_context_replay_ignores_contradiction_marker_in_shared_log() {
        use familyclaw_durable::DurableContext;
        use std::cell::Cell;

        let effects = Cell::new(0u32);
        let run = |ctx: &mut DurableContext<InMemoryJournal>| -> familyclaw_durable::Result<i32> {
            let a: i32 = ctx.step("step_a", || {
                effects.set(effects.get() + 1);
                Ok(1)
            })?;
            let b: i32 = ctx.step("step_b", || {
                effects.set(effects.get() + 1);
                Ok(a + 1)
            })?;
            Ok(b)
        };

        // Tuore ajo: kaksi askelta lokiin.
        let mut ctx = DurableContext::new(InMemoryJournal::new()).expect("ctx");
        let first = run(&mut ctx).expect("first run");
        assert_eq!(first, 2);
        assert_eq!(effects.get(), 2);

        // Dreaming kirjoittaa ristiriitamerkinnän SAMAAN lokiin askelten väliin.
        let mut journal = ctx.finish();
        mark_contradicted(&mut journal, MessageId::new()).expect("mark");

        // Rakenna konteksti uudelleen jaetun lokin päälle ja aja workflow
        // uudelleen: marker EI saa rikkoa replay-kursoria.
        let mut resumed = DurableContext::new(journal).expect("resume ctx");
        let replayed = run(&mut resumed).expect("replay must not mis-step on marker");
        assert_eq!(replayed, first, "replay-tulos identtinen");
        assert_eq!(
            effects.get(),
            2,
            "replay ei saa ajaa askelten sulkimia uudelleen (marker ohitetaan)"
        );

        // Ja merkintä on yhä luettavissa ristiriitana.
        let ids = contradicted_ids(resumed.journal()).expect("ids");
        assert_eq!(ids.len(), 1);
    }
}
