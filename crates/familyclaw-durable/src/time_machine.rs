//! Time Machine — journalin tarkastelu (inspect), haarautus (fork) ja
//! aikajanojen vertailu (diff).
//!
//! Durable-journal on append-only ja replay deterministinen — siksi historia
//! on paitsi *toistettavissa*, myös *haarautettavissa*: mikä tahansa mennyt
//! päätöspiste voidaan avata, leikata ja ajaa uudelleen muutetulla
//! jatkologiikalla ilman että alkuperäinen aikajana muuttuu tai yksikään
//! oikea sivuvaikutus toistuu.
//!
//! Kolme kerrosta:
//!
//! 1. **Inspect** — [`Timeline`] purkaa journalin ihmisluettavaksi
//!    askel-listaksi: mitä tapahtui, missä järjestyksessä, mikä onnistui ja
//!    mikä epäonnistui ("mustan laatikon" luku).
//! 2. **Fork** — [`TimeMachine::fork`] kopioi aikajanan alun uuteen
//!    journaliin ja katkaisee sen valitusta askeleesta. Haarassa historia
//!    toistuu deterministisesti leikkauspisteeseen asti, ja siitä eteenpäin
//!    suoritus on *tuoretta* — kutsuja voi ajaa vaihtoehtoisen jatkon
//!    ("mitä jos?"). Haaraan kirjataan aina [`FORK_MARKER`]-auditrivi,
//!    joten haarautuneen aikajanan alkuperä on todennettavissa.
//! 3. **Diff** — [`TimelineDiff`] vertaa kahta aikajanaa askel askeleelta ja
//!    tuottaa deterministisen, sarjallistuvan raportin: mikä pysyi samana,
//!    minkä tulos muuttui, missä aikajanat erkanivat.
//!
//! Counterfactual-ajoja varten [`DryRunRecorder`] kaappaa *aiotut* ulkoiset
//! sivuvaikutukset intenteiksi. Tyypillä **ei ole rakenteellisesti mitään
//! dispatch-polkua** — kaapattu intent ei voi koskaan saavuttaa ulkoista
//! järjestelmää tämän tyypin kautta. Tämä on sama fail-closed-periaate kuin
//! muuallakin alustassa: turvallisuus on rakenteen, ei politiikan, ominaisuus.
//!
//! ## Esimerkki: kelaa, haaraudu, vertaa
//! ```
//! use familyclaw_durable::{DurableContext, InMemoryJournal, TimeMachine};
//!
//! # fn main() -> familyclaw_durable::Result<()> {
//! // Alkuperäinen ajo: kaksi askelta.
//! let mut ctx = DurableContext::new(InMemoryJournal::new())?;
//! let a: i64 = ctx.step("load", || Ok(10))?;
//! let _b: i64 = ctx.step("apply", || Ok(a * 2))?;
//! let original = ctx.finish();
//!
//! // Haaraudu: pidä vain "load", aja "apply" uudella logiikalla.
//! let fork = TimeMachine::fork(&original, 1)?;
//! let mut alt = DurableContext::new(fork)?;
//! let a: i64 = alt.step("load", || Ok(0))?; // replay: palautuu lokista (10)
//! let _b: i64 = alt.step("apply", || Ok(a * 3))?; // tuore: uusi logiikka
//! let forked = alt.finish();
//!
//! // Vertaa aikajanat: "load" ennallaan, "apply" muuttui 20 → 30.
//! let diff = TimeMachine::diff(&original, &forked)?;
//! assert!(!diff.is_identical());
//! assert_eq!(diff.changed_count(), 1);
//! # Ok(())
//! # }
//! ```

use std::fmt::Write as _;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use familyclaw_core::time::Timestamp;

use crate::entry::{EntryKind, JournalEntry, StepId};
use crate::error::{DurableError, Result};
use crate::journal::Journal;
use crate::memory::InMemoryJournal;

/// Auditmarkerin nimi, joka kirjataan jokaisen haarautetun aikajanan alkuun
/// (tarkemmin: kopioidun prefiksin perään).
///
/// Payload kertoo montako askelta prefiksissä säilytettiin ja montako
/// lähdeaikajanassa oli yhteensä — haarautuneen aikajanan alkuperä on siis
/// aina todennettavissa lokista itsestään.
pub const FORK_MARKER: &str = "timeline_forked";

/// Yhden workflow-askeleen lopputulos tarkastelu- ja diff-näkymissä.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum StepOutcome {
    /// Askel valmistui; `output` on journaliin tallennettu tulos.
    Completed {
        /// Askeleen palauttama tulos JSON-arvona.
        output: serde_json::Value,
    },
    /// Askel epäonnistui; `error` on tallennettu virheviesti.
    Failed {
        /// Tallennettu virheviesti.
        error: String,
    },
}

impl StepOutcome {
    /// Onko lopputulos onnistunut.
    #[must_use]
    pub const fn is_completed(&self) -> bool {
        matches!(self, StepOutcome::Completed { .. })
    }
}

/// Yksi workflow-askel aikajanan tarkastelunäkymässä.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineStep {
    /// Askeleen 0-pohjainen paikka aikajanan **askelten** joukossa
    /// (markerit ja snapshotit eivät kuluta paikkoja — sama kursori kuin
    /// [`crate::DurableContext`]-replayssa).
    pub position: usize,
    /// Askeleen sekvenssipaikka journalissa.
    pub step_id: StepId,
    /// Askeleen looginen nimi.
    pub name: String,
    /// Askeleen lopputulos.
    pub outcome: StepOutcome,
    /// Rivin kirjoitushetki (vain diagnostiikkaa varten — ei vaikuta
    /// determinismiin).
    pub timestamp: Timestamp,
}

/// Journalin luettu, muuttumaton tarkastelunäkymä ("musta laatikko").
///
/// Rakennetaan [`Timeline::from_journal`]-kutsulla. Sisältää vain
/// workflow-askeleet järjestyksessä; markerien ja snapshotien määrät
/// raportoidaan erikseen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Timeline {
    /// Workflow-askeleet lisäysjärjestyksessä.
    pub steps: Vec<TimelineStep>,
    /// Marker-rivien lukumäärä lokissa (ml. sessiotila).
    pub marker_count: usize,
    /// Snapshot-rivien lukumäärä lokissa.
    pub snapshot_count: usize,
}

impl Timeline {
    /// Lukee journalin ja rakentaa tarkastelunäkymän.
    ///
    /// # Errors
    /// Vie journalin lukuvirheen läpi ([`DurableError::Io`],
    /// [`DurableError::CorruptEntry`], ...).
    pub fn from_journal<J: Journal>(journal: &J) -> Result<Self> {
        let mut steps = Vec::new();
        let mut marker_count = 0usize;
        let mut snapshot_count = 0usize;

        for entry in journal.replay_all()? {
            match &entry.kind {
                EntryKind::StepCompleted { name, output } => {
                    steps.push(TimelineStep {
                        position: steps.len(),
                        step_id: entry.step_id,
                        name: name.clone(),
                        outcome: StepOutcome::Completed {
                            output: output.clone(),
                        },
                        timestamp: entry.timestamp,
                    });
                }
                EntryKind::StepFailed { name, error } => {
                    steps.push(TimelineStep {
                        position: steps.len(),
                        step_id: entry.step_id,
                        name: name.clone(),
                        outcome: StepOutcome::Failed {
                            error: error.clone(),
                        },
                        timestamp: entry.timestamp,
                    });
                }
                kind if kind.is_snapshot() => snapshot_count += 1,
                _ => marker_count += 1,
            }
        }

        Ok(Self {
            steps,
            marker_count,
            snapshot_count,
        })
    }

    /// Askelten lukumäärä aikajanalla.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Onko aikajana tyhjä (ei yhtään workflow-askelta).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Palauttaa askeleen annetulta paikalta, jos sellainen on.
    #[must_use]
    pub fn step(&self, position: usize) -> Option<&TimelineStep> {
        self.steps.get(position)
    }

    /// Etsii ensimmäisen askeleen annetulla nimellä.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&TimelineStep> {
        self.steps.iter().find(|s| s.name == name)
    }

    /// Ihmisluettava markdown-raportti aikajanasta (CLI-/raporttikäyttöön).
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# Timeline — {} step(s), {} marker(s), {} snapshot(s)\n",
            self.steps.len(),
            self.marker_count,
            self.snapshot_count
        );
        let _ = writeln!(out, "| # | step | outcome |");
        let _ = writeln!(out, "|---|------|---------|");
        for step in &self.steps {
            let outcome = match &step.outcome {
                StepOutcome::Completed { output } => format!("ok: `{output}`"),
                StepOutcome::Failed { error } => format!("FAILED: {error}"),
            };
            let _ = writeln!(out, "| {} | `{}` | {} |", step.position, step.name, outcome);
        }
        out
    }
}

/// Yhden askelparin vertailutulos kahden aikajanan välillä.
///
/// `before` on vertailun vasen (alkuperäinen) ja `after` oikea (esim.
/// haarautettu) aikajana.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StepDiff {
    /// Sama askel, sama lopputulos molemmilla aikajanoilla.
    Unchanged {
        /// Askeleen paikka molemmilla aikajanoilla.
        position: usize,
        /// Askeleen nimi.
        name: String,
    },
    /// Sama askelnimi, mutta lopputulos muuttui (tulos tai onnistuminen).
    Changed {
        /// Askeleen paikka molemmilla aikajanoilla.
        position: usize,
        /// Askeleen nimi.
        name: String,
        /// Lopputulos alkuperäisellä aikajanalla.
        before: StepOutcome,
        /// Lopputulos vertailtavalla aikajanalla.
        after: StepOutcome,
    },
    /// Aikajanat erkanivat: samalla paikalla on eri askel (eri nimi).
    Diverged {
        /// Paikka jossa erkaantuminen havaittiin.
        position: usize,
        /// Askeleen nimi alkuperäisellä aikajanalla.
        before_name: String,
        /// Askeleen nimi vertailtavalla aikajanalla.
        after_name: String,
    },
    /// Askel on vain alkuperäisellä aikajanalla (vertailtava on lyhyempi).
    OnlyInBefore {
        /// Askeleen paikka alkuperäisellä aikajanalla.
        position: usize,
        /// Askeleen nimi.
        name: String,
    },
    /// Askel on vain vertailtavalla aikajanalla (alkuperäinen on lyhyempi).
    OnlyInAfter {
        /// Askeleen paikka vertailtavalla aikajanalla.
        position: usize,
        /// Askeleen nimi.
        name: String,
    },
}

/// Kahden aikajanan deterministinen, sarjallistuva vertailuraportti.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TimelineDiff {
    /// Askelkohtaiset vertailutulokset paikkajärjestyksessä.
    pub steps: Vec<StepDiff>,
}

impl TimelineDiff {
    /// Vertaa kahta valmiiksi luettua aikajanaa askel askeleelta.
    #[must_use]
    pub fn from_timelines(before: &Timeline, after: &Timeline) -> Self {
        let mut steps = Vec::new();
        let shared = before.len().min(after.len());

        for position in 0..shared {
            let b = &before.steps[position];
            let a = &after.steps[position];
            if b.name != a.name {
                steps.push(StepDiff::Diverged {
                    position,
                    before_name: b.name.clone(),
                    after_name: a.name.clone(),
                });
            } else if b.outcome == a.outcome {
                steps.push(StepDiff::Unchanged {
                    position,
                    name: b.name.clone(),
                });
            } else {
                steps.push(StepDiff::Changed {
                    position,
                    name: b.name.clone(),
                    before: b.outcome.clone(),
                    after: a.outcome.clone(),
                });
            }
        }

        for step in &before.steps[shared..] {
            steps.push(StepDiff::OnlyInBefore {
                position: step.position,
                name: step.name.clone(),
            });
        }
        for step in &after.steps[shared..] {
            steps.push(StepDiff::OnlyInAfter {
                position: step.position,
                name: step.name.clone(),
            });
        }

        Self { steps }
    }

    /// Ovatko aikajanat identtiset (jokainen askel [`StepDiff::Unchanged`]).
    #[must_use]
    pub fn is_identical(&self) -> bool {
        self.steps
            .iter()
            .all(|d| matches!(d, StepDiff::Unchanged { .. }))
    }

    /// Montako askelta muuttui ([`StepDiff::Changed`]).
    #[must_use]
    pub fn changed_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|d| matches!(d, StepDiff::Changed { .. }))
            .count()
    }

    /// Montako askelta on vain toisella aikajanalla
    /// ([`StepDiff::OnlyInBefore`] + [`StepDiff::OnlyInAfter`]).
    #[must_use]
    pub fn tail_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|d| {
                matches!(
                    d,
                    StepDiff::OnlyInBefore { .. } | StepDiff::OnlyInAfter { .. }
                )
            })
            .count()
    }

    /// Ensimmäinen paikka jossa aikajanat erkanivat nimeltään, jos sellainen on.
    #[must_use]
    pub fn first_divergence(&self) -> Option<usize> {
        self.steps.iter().find_map(|d| match d {
            StepDiff::Diverged { position, .. } => Some(*position),
            _ => None,
        })
    }

    /// Ihmisluettava markdown-raportti vertailusta.
    #[must_use]
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# Timeline diff — {} step(s): {} changed, {} tail-only, identical: {}\n",
            self.steps.len(),
            self.changed_count(),
            self.tail_count(),
            self.is_identical()
        );
        for diff in &self.steps {
            let line = match diff {
                StepDiff::Unchanged { position, name } => {
                    format!("- `#{position}` `{name}` — unchanged")
                }
                StepDiff::Changed {
                    position,
                    name,
                    before,
                    after,
                } => format!(
                    "- `#{position}` `{name}` — **changed**: {} → {}",
                    render_outcome(before),
                    render_outcome(after)
                ),
                StepDiff::Diverged {
                    position,
                    before_name,
                    after_name,
                } => format!("- `#{position}` — **diverged**: `{before_name}` vs `{after_name}`"),
                StepDiff::OnlyInBefore { position, name } => {
                    format!("- `#{position}` `{name}` — only in BEFORE")
                }
                StepDiff::OnlyInAfter { position, name } => {
                    format!("- `#{position}` `{name}` — only in AFTER")
                }
            };
            let _ = writeln!(out, "{line}");
        }
        out
    }
}

/// Lyhyt tekstiesitys lopputuloksesta diff-raporttiin.
fn render_outcome(outcome: &StepOutcome) -> String {
    match outcome {
        StepOutcome::Completed { output } => format!("ok `{output}`"),
        StepOutcome::Failed { error } => format!("FAILED ({error})"),
    }
}

/// Counterfactual-ajon kaappaama aiottu ulkoinen sivuvaikutus (intent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecordedIntent {
    /// Askeleen looginen nimi, jonka sisällä intent syntyi.
    pub step: String,
    /// Intentin hyötykuorma (esim. mitä *olisi* lähetetty).
    pub payload: serde_json::Value,
}

/// Dry-run-intenttien kaappari counterfactual-ajoihin.
///
/// Haarautetulla aikajanalla ajettava jatko kutsuu [`record`](Self::record)
/// -metodia siellä missä oikea suoritus dispatchaisi ulkoisen sivuvaikutuksen.
/// **Tällä tyypillä ei ole dispatch-metodia eikä mitään polkua ulkoiseen
/// järjestelmään** — kaapattu intent voidaan vain lukea ja raportoida. Näin
/// "mitä agentti olisi tehnyt" on rakenteellisesti erotettu siitä, että se
/// koskaan tapahtuisi.
#[derive(Debug, Default)]
pub struct DryRunRecorder {
    intents: Mutex<Vec<RecordedIntent>>,
}

impl DryRunRecorder {
    /// Luo tyhjän kaapparin.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Kaappaa yhden aiotun sivuvaikutuksen.
    pub fn record(&self, step: impl Into<String>, payload: serde_json::Value) {
        self.intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(RecordedIntent {
                step: step.into(),
                payload,
            });
    }

    /// Palauttaa kopion kaikista kaapatuista intenteistä kaappausjärjestyksessä.
    #[must_use]
    pub fn intents(&self) -> Vec<RecordedIntent> {
        self.intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Kaapattujen intenttien lukumäärä.
    #[must_use]
    pub fn len(&self) -> usize {
        self.intents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Onko kaappari tyhjä.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Time Machine -fasadi: inspect, fork ja diff yhden nimen alla.
///
/// Kaikki operaatiot ovat **lukevia lähdejournalin suhteen** — mikään ei
/// koskaan muuta olemassa olevaa aikajanaa (append-only-invariantti säilyy).
#[derive(Debug, Clone, Copy)]
pub struct TimeMachine;

impl TimeMachine {
    /// Lukee journalin tarkastelunäkymäksi. Katso [`Timeline::from_journal`].
    ///
    /// # Errors
    /// Vie journalin lukuvirheen läpi.
    pub fn inspect<J: Journal>(journal: &J) -> Result<Timeline> {
        Timeline::from_journal(journal)
    }

    /// Haarauttaa aikajanan: kopioi lähdejournalista `keep_steps` ensimmäistä
    /// **workflow-askelta** (sekä niitä edeltävät markerit/snapshotit) uuteen
    /// muistijournaliin ja kirjaa perään [`FORK_MARKER`]-auditrivin.
    ///
    /// Haaran päälle rakennettu [`crate::DurableContext`] toistaa säilytetyn
    /// prefiksin deterministisesti (sivuvaikutuksia ajamatta) ja jatkaa siitä
    /// tuoreena — leikkauspisteestä eteenpäin voi ajaa vaihtoehtoisen jatkon.
    /// Lähdejournal ei muutu.
    ///
    /// # Errors
    /// [`DurableError::InvalidFork`] jos `keep_steps` ylittää lähdeaikajanan
    /// askelmäärän. Muuten vie journalin luku-/kirjoitusvirheen läpi.
    pub fn fork<J: Journal>(source: &J, keep_steps: usize) -> Result<InMemoryJournal> {
        let target = InMemoryJournal::new();
        Self::fork_into(source, keep_steps, &target)?;
        Ok(target)
    }

    /// Kuten [`fork`](Self::fork), mutta kirjoittaa haaran annettuun
    /// **tyhjään** kohdejournaliin (esim. [`crate::FileJournal`] pysyvyyttä
    /// varten). Palauttaa säilytettyjen askelten määrän.
    ///
    /// # Errors
    /// [`DurableError::InvalidFork`] jos kohde ei ole tyhjä tai `keep_steps`
    /// ylittää lähdeaikajanan askelmäärän. Muuten vie journalin
    /// luku-/kirjoitusvirheen läpi.
    pub fn fork_into<J: Journal, T: Journal>(
        source: &J,
        keep_steps: usize,
        target: &T,
    ) -> Result<usize> {
        if !target.is_empty()? {
            return Err(DurableError::invalid_fork(
                "fork target journal must be empty",
            ));
        }

        let all = source.replay_all()?;
        let total_steps = all.iter().filter(|e| e.kind.is_step()).count();
        if keep_steps > total_steps {
            return Err(DurableError::invalid_fork(format!(
                "cannot keep {keep_steps} step(s): source timeline has only {total_steps}"
            )));
        }

        let mut kept = 0usize;
        for entry in all {
            if entry.kind.is_step() {
                if kept == keep_steps {
                    break;
                }
                kept += 1;
            }
            target.append(entry)?;
        }

        // Auditrivi: haaran alkuperä on todennettavissa lokista itsestään.
        target.append(JournalEntry::marker(
            StepId::new(kept as u64),
            FORK_MARKER,
            serde_json::json!({
                "kept_steps": kept,
                "source_steps": total_steps,
            }),
        ))?;

        Ok(kept)
    }

    /// Vertaa kahta aikajanaa askel askeleelta. Katso
    /// [`TimelineDiff::from_timelines`].
    ///
    /// # Errors
    /// Vie kummankin journalin lukuvirheen läpi.
    pub fn diff<A: Journal, B: Journal>(before: &A, after: &B) -> Result<TimelineDiff> {
        let b = Timeline::from_journal(before)?;
        let a = Timeline::from_journal(after)?;
        Ok(TimelineDiff::from_timelines(&b, &a))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::DurableContext;
    use serde_json::json;
    use std::cell::Cell;

    /// Apuri: kolmen askeleen ajo (load → decide → act) jossa "act" kaappaa
    /// sivuvaikutuksen laskuriin. Palauttaa valmiin journalin.
    fn three_step_run(effects: &Cell<u32>) -> InMemoryJournal {
        let mut ctx = DurableContext::new(InMemoryJournal::new()).expect("ctx");
        let amount: i64 = ctx.step("load", || Ok(100)).expect("load");
        let approved: i64 = ctx.step("decide", || Ok(amount * 2)).expect("decide");
        let _receipt: String = ctx
            .step("act", || {
                effects.set(effects.get() + 1);
                Ok(format!("sent:{approved}"))
            })
            .expect("act");
        ctx.finish()
    }

    // ---------- Inspect ----------

    #[test]
    fn timeline_lists_steps_in_order_with_outcomes() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);

        let timeline = TimeMachine::inspect(&journal).expect("inspect");
        assert_eq!(timeline.len(), 3);
        assert!(!timeline.is_empty());

        let names: Vec<&str> = timeline.steps.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["load", "decide", "act"]);

        // Paikat ovat 0-pohjaisia ja järjestyksessä.
        for (i, step) in timeline.steps.iter().enumerate() {
            assert_eq!(step.position, i);
            assert!(step.outcome.is_completed());
        }

        // Tulokset luettavissa.
        match &timeline.step(1).expect("decide").outcome {
            StepOutcome::Completed { output } => assert_eq!(output, &json!(200)),
            other @ StepOutcome::Failed { .. } => panic!("expected completed, got {other:?}"),
        }
    }

    #[test]
    fn timeline_records_failures_and_counts_non_steps() {
        let mut ctx = DurableContext::new(InMemoryJournal::new()).expect("ctx");
        let _ = ctx.step::<i32, _>("boom", || Err("kaboom".to_string()));
        ctx.snapshot(&json!({"x": 1})).expect("snapshot");
        let journal = ctx.finish();
        journal
            .append(JournalEntry::marker(StepId::new(9), "note", json!({})))
            .expect("marker");

        let timeline = TimeMachine::inspect(&journal).expect("inspect");
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline.snapshot_count, 1);
        assert_eq!(timeline.marker_count, 1);
        match &timeline.find("boom").expect("boom").outcome {
            StepOutcome::Failed { error } => assert_eq!(error, "kaboom"),
            other @ StepOutcome::Completed { .. } => panic!("expected failed, got {other:?}"),
        }
    }

    #[test]
    fn timeline_render_markdown_mentions_every_step() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let text = TimeMachine::inspect(&journal)
            .expect("inspect")
            .render_markdown();
        for name in ["load", "decide", "act"] {
            assert!(text.contains(name), "markdown must mention `{name}`");
        }
    }

    // ---------- Fork ----------

    #[test]
    fn fork_keeps_prefix_truncates_tail_and_adds_audit_marker() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);

        let fork = TimeMachine::fork(&journal, 2).expect("fork");
        let timeline = TimeMachine::inspect(&fork).expect("inspect fork");

        // Prefiksi säilyi, häntä leikkautui.
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline.steps[0].name, "load");
        assert_eq!(timeline.steps[1].name, "decide");
        assert!(timeline.find("act").is_none(), "tail must be truncated");

        // Auditmarker on lokissa.
        let entries = fork.entries();
        let marker = entries.last().expect("marker entry");
        match &marker.kind {
            EntryKind::Marker { name, payload } => {
                assert_eq!(name, FORK_MARKER);
                assert_eq!(payload["kept_steps"], json!(2));
                assert_eq!(payload["source_steps"], json!(3));
            }
            other => panic!("expected fork marker, got {other:?}"),
        }

        // Alkuperäinen aikajana ei muuttunut.
        assert_eq!(
            TimeMachine::inspect(&journal)
                .expect("inspect original")
                .len(),
            3
        );
    }

    #[test]
    fn fork_beyond_timeline_fails_closed() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let err = TimeMachine::fork(&journal, 4).expect_err("must fail");
        assert!(matches!(err, DurableError::InvalidFork { .. }));
    }

    #[test]
    fn fork_into_nonempty_target_fails_closed() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let target = InMemoryJournal::new();
        target
            .append(JournalEntry::completed(StepId::ZERO, "stale", json!(1)))
            .expect("append");
        let err = TimeMachine::fork_into(&journal, 1, &target).expect_err("must fail");
        assert!(matches!(err, DurableError::InvalidFork { .. }));
    }

    #[test]
    fn fork_at_zero_yields_empty_timeline_with_marker() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let fork = TimeMachine::fork(&journal, 0).expect("fork");
        let timeline = TimeMachine::inspect(&fork).expect("inspect");
        assert!(timeline.is_empty());
        assert_eq!(timeline.marker_count, 1, "audit marker present");
    }

    /// Ydintesti: haarautunut jatko ajaa counterfactualin — prefiksi toistuu
    /// lokista sivuvaikutuksitta, uusi jatko ajetaan tasan kerran, eikä
    /// alkuperäinen aikajana muutu.
    #[test]
    fn forked_continuation_is_counterfactual_and_leaves_original_untouched() {
        let original_effects = Cell::new(0u32);
        let journal = three_step_run(&original_effects);
        assert_eq!(
            original_effects.get(),
            1,
            "alkuperäinen act ajettiin kerran"
        );
        let original_len = journal.len().expect("len");

        // Haaraudu ennen "decide"-askelta ja aja korjattu politiikka.
        let fork = TimeMachine::fork(&journal, 1).expect("fork");
        let mut alt = DurableContext::new(fork).expect("alt ctx");

        let replay_effects = Cell::new(0u32);
        let amount: i64 = alt
            .step("load", || {
                replay_effects.set(replay_effects.get() + 1);
                Ok(0)
            })
            .expect("load replay");
        assert_eq!(amount, 100, "prefiksi palautuu lokista");
        assert_eq!(replay_effects.get(), 0, "replay ei aja suljinta");

        // Counterfactual: uusi decide-politiikka + dry-run act.
        let recorder = DryRunRecorder::new();
        let approved: i64 = alt.step("decide", || Ok(amount / 2)).expect("decide alt");
        assert_eq!(approved, 50);
        let _receipt: String = alt
            .step("act", || {
                recorder.record("act", json!({"would_send": approved}));
                Ok(format!("dry:{approved}"))
            })
            .expect("act alt");

        // Intent kaapattiin — mitään ei dispatchattu (tyypillä ei ole polkua).
        assert_eq!(recorder.len(), 1);
        assert_eq!(recorder.intents()[0].payload, json!({"would_send": 50}));

        // Alkuperäinen aikajana on täsmälleen ennallaan.
        assert_eq!(journal.len().expect("len"), original_len);
        assert_eq!(
            original_effects.get(),
            1,
            "ei uusia oikeita sivuvaikutuksia"
        );
    }

    #[test]
    fn fork_into_file_journal_replays_deterministically() {
        use crate::file::FileJournal;

        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-durable-tm-fork-{}-{:?}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        let _ = std::fs::remove_file(&path);

        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);

        // Haarauta pysyvään FileJournaliin.
        let target = FileJournal::open(&path).expect("open target");
        let kept = TimeMachine::fork_into(&journal, 2, &target).expect("fork_into");
        assert_eq!(kept, 2);
        drop(target);

        // "Restart": avaa haara uudella kahvalla ja jatka siitä.
        let reopened = FileJournal::open(&path).expect("reopen");
        let mut ctx = DurableContext::new(reopened).expect("ctx");
        let a: i64 = ctx.step("load", || Ok(0)).expect("load");
        let b: i64 = ctx.step("decide", || Ok(0)).expect("decide");
        assert_eq!((a, b), (100, 200), "prefiksi palautuu lokista levyltäkin");
        assert!(!ctx.is_replaying());

        let _ = std::fs::remove_file(&path);
    }

    // ---------- Diff ----------

    #[test]
    fn diff_of_identical_timelines_is_identical() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let diff = TimeMachine::diff(&journal, &journal).expect("diff");
        assert!(diff.is_identical());
        assert_eq!(diff.changed_count(), 0);
        assert_eq!(diff.tail_count(), 0);
        assert_eq!(diff.first_divergence(), None);
        assert_eq!(diff.steps.len(), 3);
    }

    #[test]
    fn diff_reports_changed_steps_and_tails() {
        let effects = Cell::new(0u32);
        let original = three_step_run(&effects);

        // Haara: sama load, eri decide/act, plus ylimääräinen askel.
        let fork = TimeMachine::fork(&original, 1).expect("fork");
        let mut alt = DurableContext::new(fork).expect("alt");
        let amount: i64 = alt.step("load", || Ok(0)).expect("load");
        let approved: i64 = alt.step("decide", || Ok(amount / 2)).expect("decide");
        let _r: String = alt
            .step("act", || Ok(format!("dry:{approved}")))
            .expect("act");
        let _extra: i64 = alt.step("audit", || Ok(1)).expect("audit");
        let forked = alt.finish();

        let diff = TimeMachine::diff(&original, &forked).expect("diff");
        assert!(!diff.is_identical());
        assert_eq!(diff.changed_count(), 2, "decide ja act muuttuivat");
        assert_eq!(diff.tail_count(), 1, "audit vain haarassa");
        assert_eq!(diff.first_divergence(), None, "nimet eivät erkaantuneet");

        assert!(matches!(
            &diff.steps[0],
            StepDiff::Unchanged { name, .. } if name == "load"
        ));
        assert!(matches!(
            &diff.steps[1],
            StepDiff::Changed { name, .. } if name == "decide"
        ));
        assert!(matches!(
            &diff.steps[3],
            StepDiff::OnlyInAfter { name, .. } if name == "audit"
        ));
    }

    #[test]
    fn diff_detects_divergence_by_name() {
        let mut a = DurableContext::new(InMemoryJournal::new()).expect("a");
        let _ = a.step("x", || Ok::<_, String>(1)).expect("x");
        let a = a.finish();

        let mut b = DurableContext::new(InMemoryJournal::new()).expect("b");
        let _ = b.step("y", || Ok::<_, String>(1)).expect("y");
        let b = b.finish();

        let diff = TimeMachine::diff(&a, &b).expect("diff");
        assert_eq!(diff.first_divergence(), Some(0));
        assert!(matches!(
            &diff.steps[0],
            StepDiff::Diverged { before_name, after_name, .. }
                if before_name == "x" && after_name == "y"
        ));
    }

    #[test]
    fn diff_treats_failure_change_as_changed() {
        let mut a = DurableContext::new(InMemoryJournal::new()).expect("a");
        let _ = a.step("risky", || Ok::<_, String>(1)).expect("ok run");
        let a = a.finish();

        let mut b = DurableContext::new(InMemoryJournal::new()).expect("b");
        let _ = b.step::<i32, _>("risky", || Err("nope".to_string()));
        let b = b.finish();

        let diff = TimeMachine::diff(&a, &b).expect("diff");
        assert_eq!(diff.changed_count(), 1);
        match &diff.steps[0] {
            StepDiff::Changed { before, after, .. } => {
                assert!(before.is_completed());
                assert!(!after.is_completed());
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn diff_render_markdown_summarizes() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let text = TimeMachine::diff(&journal, &journal)
            .expect("diff")
            .render_markdown();
        assert!(text.contains("identical: true"));
        assert!(text.contains("unchanged"));
    }

    #[test]
    fn diff_serializes_to_json() {
        let effects = Cell::new(0u32);
        let journal = three_step_run(&effects);
        let diff = TimeMachine::diff(&journal, &journal).expect("diff");
        let json = serde_json::to_string(&diff).expect("serialize");
        assert!(json.contains("\"kind\":\"unchanged\""));
    }

    // ---------- DryRunRecorder ----------

    #[test]
    fn dry_run_recorder_captures_in_order() {
        let recorder = DryRunRecorder::new();
        assert!(recorder.is_empty());
        recorder.record("first", json!({"n": 1}));
        recorder.record("second", json!({"n": 2}));
        assert_eq!(recorder.len(), 2);
        let intents = recorder.intents();
        assert_eq!(intents[0].step, "first");
        assert_eq!(intents[1].step, "second");
        assert_eq!(intents[1].payload, json!({"n": 2}));
    }
}
