//! [`DurableContext`] — deterministisen replayn `step`-API.
//!
//! Tämä on perheen #1 kipupisteen (muistin epäjatkuvuus) rakenteellinen
//! ratkaisu (design §2.1). Workflow kääritään askeliin
//! ([`step`](DurableContext::step)). Kun konteksti rakennetaan olemassa olevan
//! journalin päälle, jo suoritetut askeleet **palautetaan lokista ajamatta
//! niiden sulkimia uudelleen** — eli sivuvaikutukset eivät toistu, mutta tulos
//! on sama. Kaatumisen jälkeen workflow jatkuu täsmälleen siitä mihin se jäi.
//!
//! ## Determinismin invariantti
//! Koodin täytyy tuottaa samat askeleet (sama nimi, sama järjestys) joka
//! ajolla. Jos replay-koodi pyytää askeleen jonka nimi ei vastaa journalissa
//! samalla paikalla olevaa, [`step`](DurableContext::step) palauttaa
//! [`DurableError::NondeterministicReplay`]:n sen sijaan että jatkaisi
//! hiljaa väärin.

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::entry::{EntryKind, JournalEntry, StepId};
use crate::error::{DurableError, Result};
use crate::journal::Journal;

/// Deterministisen replayn suorituskonteksti yhden journalin yli.
///
/// Geneerinen journal-toteutuksen `J` yli, joten sama logiikka toimii
/// [`crate::InMemoryJournal`]- ja [`crate::FileJournal`]-taustalla.
#[derive(Debug)]
pub struct DurableContext<J: Journal> {
    journal: J,
    /// Aiemmin tallennetut rivit, joiden yli replay etenee. Vain ne rivit
    /// jotka liittyvät askeleeseen (StepCompleted/StepFailed) — snapshotit JA
    /// markerit suodatetaan pois, koska ne eivät ole `step`-kutsuja.
    replay: Vec<JournalEntry>,
    /// Kuinka monta `step`-kutsua on jo tehty tällä kontekstilla. Toimii sekä
    /// replay-kursorina että seuraavan askeleen sekvenssipaikkana.
    cursor: usize,
}

impl<J: Journal> DurableContext<J> {
    /// Rakentaa kontekstin journalin päälle, lataten aiemmat askeleet
    /// replay-pohjaksi.
    ///
    /// # Errors
    /// Vie virheen läpi jos journalin luku epäonnistuu
    /// (esim. [`DurableError::CorruptEntry`]).
    pub fn new(journal: J) -> Result<Self> {
        let all = journal.replay_all()?;
        // Säilytä vain askel-rivit (StepCompleted/StepFailed) replay-kursoria
        // varten. Snapshotit (optimointi) ja markerit (esim. dreaming-vaiheen
        // ristiriitamerkinnät) EIVÄT ole `step`-kutsuja, joten ne eivät kuluta
        // kursoria — näin sama jaettu loki voi kantaa molempia ilman että
        // marker-rivi näyttäytyy workflow-askeleena ja laukaisee
        // NondeterministicReplay-virheen.
        let replay: Vec<JournalEntry> = all.into_iter().filter(|e| e.kind.is_step()).collect();
        Ok(Self {
            journal,
            replay,
            cursor: 0,
        })
    }

    /// Onko konteksti tällä hetkellä toistamassa aiemmin tallennettuja askelia.
    ///
    /// `true` niin kauan kuin kursori ei ole ohittanut tallennettuja rivejä.
    #[must_use]
    pub fn is_replaying(&self) -> bool {
        self.cursor < self.replay.len()
    }

    /// Kuinka monta askelta on jo suoritettu tai toistettu.
    #[must_use]
    pub fn steps_taken(&self) -> usize {
        self.cursor
    }

    /// Seuraavan askeleen sekvenssipaikka.
    #[must_use]
    pub fn next_step_id(&self) -> StepId {
        StepId::new(self.cursor as u64)
    }

    /// Suorittaa nimetyn askeleen kerran-ja-vain-kerran-semantiikalla.
    ///
    /// - **Tuore ajo:** suljin `f` ajetaan, tulos sarjallistuu ja kirjoitetaan
    ///   journaliin ennen paluuta.
    /// - **Replay:** jos tällä paikalla on jo tallennettu rivi, suljinta `f`
    ///   **ei ajeta** — tallennettu tulos jäsennetään ja palautetaan (tai
    ///   tallennettu virhe palautetaan).
    ///
    /// Tämä takaa että askeleen sivuvaikutukset (verkkokutsu, tiedostokirjoitus)
    /// tapahtuvat tasan kerran koko workflow'n elinkaaren yli, vaikka prosessi
    /// kaatuisi ja käynnistyisi uudelleen kesken.
    ///
    /// # Errors
    /// - [`DurableError::NondeterministicReplay`] jos `name` ei vastaa
    ///   journalissa tällä paikalla olevaa askelta.
    /// - [`DurableError::StepFailed`] jos suljin palautti virheen (tuore ajo)
    ///   tai jos tallennettu rivi oli epäonnistuminen (replay).
    /// - [`DurableError::Serde`] jos tuloksen sarjallistus/jäsennys epäonnistuu.
    /// - [`DurableError::Io`] jos journalin kirjoitus epäonnistuu.
    pub fn step<T, F>(&mut self, name: &str, f: F) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> std::result::Result<T, String>,
    {
        let index = self.cursor as u64;

        // Replay-haara: tällä paikalla on jo tallennettu rivi.
        if let Some(entry) = self.replay.get(self.cursor) {
            let recorded_name = entry.step_name().unwrap_or_default();
            if recorded_name != name {
                return Err(DurableError::NondeterministicReplay {
                    index,
                    expected: name.to_string(),
                    found: recorded_name.to_string(),
                });
            }
            let result = match &entry.kind {
                EntryKind::StepCompleted { output, .. } => {
                    let value: T = serde_json::from_value(output.clone())?;
                    Ok(value)
                }
                EntryKind::StepFailed { error, .. } => {
                    Err(DurableError::step_failed(name, error.clone()))
                }
                // Replay-vektori sisältää vain askel-rivejä (`is_step`),
                // joten snapshotit/markerit (ja mahdolliset tulevat ei-askel-
                // lajit) on jo suodatettu pois `new`:ssä. Tätä haaraa ei pitäisi
                // koskaan saavuttaa — mutta käsitellään silti ilman paniikkia.
                other => Err(DurableError::NondeterministicReplay {
                    index,
                    expected: name.to_string(),
                    found: format!("<non-step entry: {}>", non_step_label(other)),
                }),
            };
            self.cursor += 1;
            return result;
        }

        // Tuore-ajo-haara: suljin ajetaan kerran.
        let step_id = StepId::new(index);
        match f() {
            Ok(value) => {
                let output = serde_json::to_value(&value)?;
                self.journal
                    .append(JournalEntry::completed(step_id, name, output))?;
                self.cursor += 1;
                Ok(value)
            }
            Err(message) => {
                // Kirjaa epäonnistuminen jotta replay palauttaa saman virheen
                // ajamatta sivuvaikutuksia uudelleen.
                self.journal
                    .append(JournalEntry::failed(step_id, name, message.clone()))?;
                self.cursor += 1;
                Err(DurableError::step_failed(name, message))
            }
        }
    }

    /// Kirjoittaa snapshotin nykytilasta nykyiselle sekvenssipaikalle.
    ///
    /// Snapshot ei kuluta `step`-kursoria eikä keskeytä replayta — se on
    /// lisämerkintä lokiin auditointia/optimointia varten.
    ///
    /// # Errors
    /// [`DurableError::Io`]/[`DurableError::Serde`] jos kirjoitus epäonnistuu.
    pub fn snapshot<S: Serialize>(&mut self, state: &S) -> Result<()> {
        let value = serde_json::to_value(state)?;
        self.journal.snapshot(self.next_step_id(), value)
    }

    /// Kuluttaa kontekstin ja palauttaa taustalla olevan journalin.
    ///
    /// Tätä käytetään kun workflow on ajettu loppuun (tai "kaatumisen"
    /// simuloimiseksi testeissä): journal voidaan ottaa talteen ja rakentaa
    /// uusi konteksti sen päälle replayta varten.
    #[must_use]
    pub fn finish(self) -> J {
        self.journal
    }

    /// Palauttaa viittauksen taustalla olevaan journaliin (esim. rivien
    /// tarkasteluun testeissä).
    #[must_use]
    pub fn journal(&self) -> &J {
        &self.journal
    }
}

/// Lyhyt diagnostiikkaleima ei-askel-rivilajille (snapshot/marker/tuleva laji).
///
/// Käytetään vain "ei pitäisi tapahtua" -virhepolulla: replay-vektori on jo
/// suodatettu pelkkiin askeliin, joten tätä kutsutaan käytännössä ei koskaan.
fn non_step_label(kind: &EntryKind) -> &'static str {
    if kind.is_snapshot() {
        "snapshot"
    } else if kind.is_marker() {
        "marker"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::InMemoryJournal;
    use std::cell::Cell;

    /// Apuri: tuore konteksti tyhjän muistijournalin päälle.
    fn fresh() -> DurableContext<InMemoryJournal> {
        DurableContext::new(InMemoryJournal::new()).expect("new context")
    }

    /// Pieni RAII-temp-tiedosto ilman ulkoisia crateja.
    struct TempPath(std::path::PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "familyclaw-durable-ctx-{tag}-{}-{:?}.jsonl",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            let _ = std::fs::remove_file(&p);
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Kolmiaskelinen testityö: 10 → +5 → ×2 = 30. Laskuri kirjaa montako
    /// kertaa sulkimet OIKEASTI ajetaan (replay ei kasvata sitä).
    fn three_step_workflow<J: Journal>(
        ctx: &mut DurableContext<J>,
        effects: &Cell<u32>,
    ) -> Result<i32> {
        let a: i32 = ctx.step("step_a", || {
            effects.set(effects.get() + 1);
            Ok(10)
        })?;
        let b: i32 = ctx.step("step_b", || {
            effects.set(effects.get() + 1);
            Ok(a + 5)
        })?;
        let c: i32 = ctx.step("step_c", || {
            effects.set(effects.get() + 1);
            Ok(b * 2)
        })?;
        Ok(c)
    }

    #[test]
    fn fresh_step_runs_closure_and_records() {
        let mut ctx = fresh();
        let out: i32 = ctx.step("add", || Ok(2 + 3)).expect("step ok");
        assert_eq!(out, 5);
        assert_eq!(ctx.steps_taken(), 1);

        let entries = ctx.journal().entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].step_name(), Some("add"));
    }

    #[test]
    fn sequential_steps_increment_step_ids() {
        let mut ctx = fresh();
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        let _ = ctx.step("b", || Ok::<_, String>(2)).expect("b");
        let _ = ctx.step("c", || Ok::<_, String>(3)).expect("c");
        let entries = ctx.journal().entries();
        assert_eq!(entries[0].step_id, StepId::new(0));
        assert_eq!(entries[1].step_id, StepId::new(1));
        assert_eq!(entries[2].step_id, StepId::new(2));
    }

    #[test]
    fn step_failure_is_recorded_and_returned() {
        let mut ctx = fresh();
        let res: Result<i32> = ctx.step("boom", || Err("kaboom".to_string()));
        match res {
            Err(DurableError::StepFailed { step, message }) => {
                assert_eq!(step, "boom");
                assert_eq!(message, "kaboom");
            }
            other => panic!("expected StepFailed, got {other:?}"),
        }
        // Epäonnistuminen on lokissa.
        let entries = ctx.journal().entries();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].kind, EntryKind::StepFailed { .. }));
    }

    /// Ydintesti: aja workflow puoliksi sivuvaikutuksilla, "kaada", rakenna
    /// uusi konteksti samasta journalista, replay → side-effectit EIVÄT toistu,
    /// tulos sama.
    #[test]
    fn replay_does_not_repeat_side_effects() {
        // Sivuvaikutuslaskuri: jokainen sulkimen oikea suoritus kasvattaa tätä.
        let effects = Cell::new(0u32);

        let run_workflow = |ctx: &mut DurableContext<InMemoryJournal>| -> Result<i32> {
            let a: i32 = ctx.step("step_a", || {
                effects.set(effects.get() + 1);
                Ok(10)
            })?;
            let b: i32 = ctx.step("step_b", || {
                effects.set(effects.get() + 1);
                Ok(a + 5)
            })?;
            let c: i32 = ctx.step("step_c", || {
                effects.set(effects.get() + 1);
                Ok(b * 2)
            })?;
            Ok(c)
        };

        // --- Ensimmäinen (täysi) ajo ---
        let journal = InMemoryJournal::new();
        let mut ctx = DurableContext::new(journal).expect("ctx 1");
        let first = run_workflow(&mut ctx).expect("first run");
        assert_eq!(first, 30);
        assert_eq!(effects.get(), 3, "kolme sivuvaikutusta tuoreessa ajossa");

        // Ota journal talteen kuin se olisi levyllä kaatumisen yli.
        let journal = ctx.finish();
        assert_eq!(journal.len().expect("len"), 3);

        // --- Replay: uusi konteksti samasta journalista ---
        let mut ctx2 = DurableContext::new(journal).expect("ctx 2");
        let replayed = run_workflow(&mut ctx2).expect("replay run");

        // Tulos identtinen JA yhtään uutta sivuvaikutusta ei syntynyt.
        assert_eq!(replayed, first);
        assert_eq!(
            effects.get(),
            3,
            "replay ei saa ajaa sulkimia uudelleen — ei uusia sivuvaikutuksia"
        );
    }

    /// Kaadu kesken: vain osa askeleista on lokissa, replay täyttää loput.
    #[test]
    fn partial_journal_resumes_from_where_it_left_off() {
        let effects = Cell::new(0u32);

        // Vaihe 1: aja vain kaksi ensimmäistä askelta, sitten "kaada".
        let journal = InMemoryJournal::new();
        let mut ctx = DurableContext::new(journal).expect("ctx");
        let _ = ctx
            .step("a", || {
                effects.set(effects.get() + 1);
                Ok::<_, String>(1)
            })
            .expect("a");
        let _ = ctx
            .step("b", || {
                effects.set(effects.get() + 1);
                Ok::<_, String>(2)
            })
            .expect("b");
        let journal = ctx.finish();
        assert_eq!(effects.get(), 2);
        assert_eq!(journal.len().expect("len"), 2);

        // Vaihe 2: jatka — a ja b toistetaan lokista (ei sivuvaikutusta),
        // c ajetaan tuoreena (yksi uusi sivuvaikutus).
        let mut ctx2 = DurableContext::new(journal).expect("ctx 2");
        assert!(ctx2.is_replaying());
        let a: i32 = ctx2
            .step("a", || {
                effects.set(effects.get() + 1);
                Ok(1)
            })
            .expect("a replay");
        let b: i32 = ctx2
            .step("b", || {
                effects.set(effects.get() + 1);
                Ok(2)
            })
            .expect("b replay");
        assert!(!ctx2.is_replaying(), "kursori ohitti tallennetut rivit");
        let c: i32 = ctx2
            .step("c", || {
                effects.set(effects.get() + 1);
                Ok(a + b)
            })
            .expect("c fresh");
        assert_eq!(c, 3);
        // a+b toistettiin (0 uutta), c tuore (+1) → yhteensä 3.
        assert_eq!(effects.get(), 3);
    }

    #[test]
    fn nondeterministic_step_name_is_detected() {
        // Lokissa askel "a"; replay-koodi pyytää "b" samalla paikalla.
        let mut ctx = fresh();
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        let journal = ctx.finish();

        let mut ctx2 = DurableContext::new(journal).expect("ctx2");
        let err = ctx2
            .step::<i32, _>("b", || Ok(1))
            .expect_err("name mismatch must error");
        match err {
            DurableError::NondeterministicReplay {
                index,
                expected,
                found,
            } => {
                assert_eq!(index, 0);
                assert_eq!(expected, "b");
                assert_eq!(found, "a");
            }
            other => panic!("expected NondeterministicReplay, got {other:?}"),
        }
    }

    #[test]
    fn recorded_failure_replays_as_failure_without_rerun() {
        let ran = Cell::new(false);
        // Tuore ajo: askel epäonnistuu, kirjautuu virheenä.
        let journal = InMemoryJournal::new();
        let mut ctx = DurableContext::new(journal).expect("ctx");
        let _ = ctx.step::<i32, _>("risky", || Err("nope".to_string()));
        let journal = ctx.finish();

        // Replay: sama askel palauttaa saman virheen ajamatta suljinta.
        let mut ctx2 = DurableContext::new(journal).expect("ctx2");
        let res: Result<i32> = ctx2.step("risky", || {
            ran.set(true);
            Ok(99)
        });
        assert!(
            !ran.get(),
            "epäonnistunutta askelta ei aja uudelleen replayssa"
        );
        match res {
            Err(DurableError::StepFailed { message, .. }) => assert_eq!(message, "nope"),
            other => panic!("expected StepFailed, got {other:?}"),
        }
    }

    #[test]
    fn finish_consumes_context_and_returns_journal() {
        // finish() siirtää omistuksen journaliin, joten kontekstia ei voi enää
        // käyttää (käännösaikainen takuu, ei ajonaikainen lippu).
        let mut ctx = fresh();
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        let journal = ctx.finish();
        assert_eq!(journal.len().expect("len"), 1);
    }

    #[test]
    fn snapshot_does_not_consume_step_cursor() {
        let mut ctx = fresh();
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        ctx.snapshot(&serde_json::json!({"acc": 1}))
            .expect("snapshot");
        let _ = ctx.step("b", || Ok::<_, String>(2)).expect("b");

        // Lokissa: a (step0), snapshot, b (step1). Snapshot ei kuluta kursoria.
        let entries = ctx.journal().entries();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].step_name(), Some("a"));
        assert!(entries[1].kind.is_snapshot());
        assert_eq!(entries[2].step_name(), Some("b"));
        assert_eq!(entries[2].step_id, StepId::new(1));
        assert_eq!(ctx.steps_taken(), 2);
    }

    #[test]
    fn snapshot_is_ignored_during_replay_cursor() {
        // Lokissa snapshot askelten välissä ei saa rikkoa replayn nimimatchia.
        let mut ctx = fresh();
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        ctx.snapshot(&serde_json::json!({"x": 1})).expect("snap");
        let _ = ctx.step("b", || Ok::<_, String>(2)).expect("b");
        let journal = ctx.finish();

        // Replay: a ja b toistuvat oikein vaikka snapshot on välissä lokissa.
        let mut ctx2 = DurableContext::new(journal).expect("ctx2");
        let a: i32 = ctx2.step("a", || Ok(0)).expect("a replay");
        let b: i32 = ctx2.step("b", || Ok(0)).expect("b replay");
        assert_eq!(a, 1);
        assert_eq!(b, 2);
    }

    #[test]
    fn marker_in_log_does_not_consume_step_cursor_or_break_replay() {
        // Loki jossa askel "a", marker (ei-askel), askel "b". Markerin EI saa
        // näkyä replay-kursorissa eikä aiheuttaa NondeterministicReplay:ta.
        let mut ctx = fresh();
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        ctx.journal
            .append(JournalEntry::marker(
                StepId::new(99),
                "memory_contradicted",
                serde_json::json!({"memory": "x"}),
            ))
            .expect("append marker");
        let _ = ctx.step("b", || Ok::<_, String>(2)).expect("b");
        let journal = ctx.finish();

        // Lokissa kolme riviä mutta vain kaksi askelta.
        assert_eq!(journal.len().expect("len"), 3);

        // Replay: a ja b toistuvat oikein vaikka marker on välissä lokissa.
        let mut ctx2 = DurableContext::new(journal).expect("ctx2");
        // Replay-kursori näkee vain kaksi askelta (marker suodatettu pois).
        assert!(ctx2.is_replaying());
        let a: i32 = ctx2.step("a", || Ok(0)).expect("a replay");
        let b: i32 = ctx2.step("b", || Ok(0)).expect("b replay");
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert!(!ctx2.is_replaying());
        assert_eq!(ctx2.steps_taken(), 2);
    }

    #[test]
    fn next_step_id_tracks_cursor() {
        let mut ctx = fresh();
        assert_eq!(ctx.next_step_id(), StepId::new(0));
        let _ = ctx.step("a", || Ok::<_, String>(1)).expect("a");
        assert_eq!(ctx.next_step_id(), StepId::new(1));
    }

    /// Ydin-integraatio (review issue #8): kaatumiskestävyys päästä päähän
    /// `DurableContext`:n + `FileJournal`:in **yhdistelmänä**, kun viimeinen
    /// rivi typistyi kaatumisessa. Resumeen jälkeen säilyneet askeleet eivät
    /// toista sivuvaikutuksia, typistynyt askel ajetaan tuoreena tasan kerran,
    /// ja lopputulos vastaa kaatumatonta ajoa (= 30).
    #[test]
    fn file_journal_torn_last_line_resumes_on_correct_step() {
        use crate::file::FileJournal;
        use std::io::Write;

        let tmp = TempPath::new("torn");

        // --- Vaihe 1: aja kaikki kolme askelta FileJournaliin, sitten "kaada". ---
        let effects = Cell::new(0u32);
        {
            let mut ctx =
                DurableContext::new(FileJournal::open(tmp.path()).expect("open 1")).expect("ctx 1");
            assert_eq!(three_step_workflow(&mut ctx, &effects).expect("first"), 30);
            assert_eq!(effects.get(), 3, "kolme tuoretta sivuvaikutusta");
        }

        // --- Kaatuminen: revi viimeinen rivi (step_c): jätä kaksi ehjää riviä +
        //     vajaa (rivinvaihdoton) tynkä = klassinen torn last line. ---
        {
            let contents = std::fs::read_to_string(tmp.path()).expect("read");
            let mut lines: Vec<&str> = contents.lines().collect();
            assert_eq!(lines.len(), 3, "kolme riviä ennen revintää");
            lines.pop();
            let mut f = std::fs::File::create(tmp.path()).expect("recreate");
            for l in &lines {
                writeln!(f, "{l}").expect("write line");
            }
            write!(f, "{{\"step_id\":2,\"timestamp\":\"2026").expect("write partial");
            f.flush().expect("flush");
        }

        // --- Vaihe 2: resume. step_a + step_b toistuvat lokista (ei uutta
        //     sivuvaikutusta), step_c ajetaan tuoreena TASAN kerran. ---
        let resumed_effects = Cell::new(0u32);
        let mut ctx2 =
            DurableContext::new(FileJournal::open(tmp.path()).expect("open 2")).expect("ctx 2");
        assert!(ctx2.is_replaying(), "kaksi ehjää askelta lokissa");
        let resumed = three_step_workflow(&mut ctx2, &resumed_effects).expect("resume");

        assert_eq!(resumed, 30, "lopputulos vastaa kaatumatonta ajoa");
        assert_eq!(
            resumed_effects.get(),
            1,
            "vain typistynyt step_c ajettiin uudelleen; step_a/step_b tulivat lokista"
        );
    }

    #[test]
    fn complex_value_roundtrips_through_replay() {
        use serde::{Deserialize, Serialize};
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        struct Payload {
            id: u32,
            tags: Vec<String>,
        }

        let made = Payload {
            id: 7,
            tags: vec!["a".into(), "b".into()],
        };

        let journal = InMemoryJournal::new();
        let mut ctx = DurableContext::new(journal).expect("ctx");
        let out = ctx
            .step("build", || Ok::<_, String>(made.clone()))
            .expect("build");
        assert_eq!(out, made);
        let journal = ctx.finish();

        // Replay palauttaa identtisen rakenteen jäsennettynä lokista.
        let mut ctx2 = DurableContext::new(journal).expect("ctx2");
        let replayed: Payload = ctx2
            .step("build", || {
                Ok(Payload {
                    id: 0,
                    tags: vec![],
                })
            })
            .expect("replay");
        assert_eq!(replayed, made);
    }
}
