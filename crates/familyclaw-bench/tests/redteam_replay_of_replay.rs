//! RED-TEAM: kaatuminen KESKEN replayn — toistuvasti (resume the resume).
//!
//! Hyökkäys (design §5, ensimmäinen luoti): *"crash during replay-of-replay
//! (resume the resume)"*. Käynnistä tehtävä, kaada, käynnistä uudelleen
//! replay-tilaan, **kaada uudelleen kesken replayn**, käynnistä kolmannen
//! kerran — ja silti jatkuvuuden täytyy pitää: lopputila vastaa kaatumatonta
//! ajoa ja sivuvaikutukset (muistikirjaukset) tapahtuvat **tasan kerran**.
//!
//! Tämä ajetaan **aidon prosessirajan yli**: jokainen "kaatuminen" on erillinen
//! `continuity_daemon`-prosessi joka poistuu exit-koodilla 137 (SIGKILL-tyyli).
//! Kello injektoidaan joka kutsuun → deterministinen.
//!
//! Ydinkysymys: voiko replayn KESKEYTTÄVÄ kaatuminen — toistettuna — turmella
//! journalin tai aiheuttaa kaksoiskirjauksen / askelen katoamisen?

use std::path::{Path, PathBuf};
use std::process::Command;

/// Paikantaa `continuity_daemon`-binäärin saman profiilin kansiosta.
fn daemon_bin() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let deps = exe.parent().expect("deps dir");
    let profile_dir = deps.parent().expect("profile dir");
    let mut bin = profile_dir.join("continuity_daemon");
    if cfg!(windows) {
        bin.set_extension("exe");
    }
    assert!(
        bin.exists(),
        "continuity_daemon binary not found at {} — build it first",
        bin.display()
    );
    bin
}

/// Kiinteä injektoitu kello (RFC 3339) — reprodusoitavuus.
const CLOCK: &str = "2024-05-29T18:13:20+00:00"; // = unix 1_717_000_000

/// Uniikki väliaikaishakemisto tälle hyökkäysajolle.
fn tempdir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "familyclaw-redteam-replay2-{}-{}-{tag}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// Ajaa daemonin alikomennon ja palauttaa (`exit_ok`, stdout, stderr).
fn run(bin: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(bin).args(args).output().expect("spawn daemon");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Poimii stdoutin `RESULT <json>`-rivin jäsennettynä Valueksi.
fn result_json(stdout: &str) -> serde_json::Value {
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("RESULT "))
        .unwrap_or_else(|| panic!("no RESULT line in stdout: {stdout:?}"));
    serde_json::from_str(line).expect("parse RESULT json")
}

/// Laskee journalin `step_completed`-rivit (revityt vajaat rivit eivät jäsenny).
fn count_completed_lines(journal: &Path) -> usize {
    let Ok(contents) = std::fs::read_to_string(journal) else {
        return 0;
    };
    contents
        .lines()
        .filter(|l| {
            // Journalin rivi: {"step_id":N,"timestamp":..,"kind":{"kind":"step_completed",..}}
            // — `kind` on SISÄKKÄINEN objekti, joten luetaan `kind.kind`.
            serde_json::from_str::<serde_json::Value>(l)
                .ok()
                .and_then(|v| {
                    v.get("kind")
                        .and_then(|k| k.get("kind"))
                        .and_then(|k| k.as_str().map(str::to_owned))
                })
                .as_deref()
                == Some("step_completed")
        })
        .count()
}

/// Laskee tehtävälle `task` kuuluvat muistot store-JSON:sta (tag `task:<id>`).
///
/// Lukee storen raakana JSON:na — ei luota daemonin omaan laskuriin, jotta
/// kaksoiskirjaus paljastuu vaikka daemon raportoisi "clean".
fn count_task_memories(store: &Path, task: &str) -> usize {
    let Ok(contents) = std::fs::read_to_string(store) else {
        return 0;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return 0;
    };
    let want = format!("task:{task}");
    // Store voi olla joko { "<id>": <memory> } tai { "memories": {...} } tms.
    // Etsitään rekursiivisesti kaikki objektit joilla on oikea tag.
    let mut count = 0usize;
    let mut stack = vec![value];
    while let Some(node) = stack.pop() {
        match node {
            serde_json::Value::Object(map) => {
                let has_tag = map
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .is_some_and(|arr| arr.iter().any(|t| t.as_str() == Some(&want)));
                if has_tag {
                    count += 1;
                }
                for (_k, v) in map {
                    stack.push(v);
                }
            }
            serde_json::Value::Array(arr) => stack.extend(arr),
            _ => {}
        }
    }
    count
}

/// Args-helperi: yhteinen `start`-runko.
fn start_args<'a>(
    journal: &'a str,
    store: &'a str,
    task: &'a str,
    steps: &'a str,
    crash_at: Option<&'a str>,
) -> Vec<&'a str> {
    let mut v = vec![
        "start",
        "--journal",
        journal,
        "--store",
        store,
        "--task",
        task,
        "--steps",
        steps,
        "--clock",
        CLOCK,
    ];
    if let Some(point) = crash_at {
        v.push("--crash-at");
        v.push(point);
    }
    v
}

/// HYÖKKÄYS: kaada KESKEN replayn kahdesti peräkkäin, sitten resume.
///
/// Sekvenssi (jokainen rivi = erillinen prosessi aidon rajan yli):
/// 1. `start --crash-at mid_write`  → osittainen journal (2 ehjää + torn).
/// 2. `start --crash-at mid_replay` → re-enter replay, kaadu kesken (#2 crash).
/// 3. `start --crash-at mid_replay` → re-enter replay UUDELLEEN, kaadu (#3 crash).
/// 4. `resume`                       → täytyy jatkua puhtaasti, sivuvaikutukset 1×.
#[test]
fn replay_of_replay_thrice_resumes_clean_with_side_effects_once() {
    let bin = daemon_bin();
    let dir = tempdir("thrice");
    let journal = dir.join("j.jsonl");
    let store = dir.join("s.json");
    let jp = journal.to_string_lossy().into_owned();
    let sp = store.to_string_lossy().into_owned();
    let task = "replay2-attack";
    let steps = "3";

    // ── Crash #1: mid_write → torn last line; steps 0,1 committed. ──
    let (ok1, _o1, e1) = run(&bin, &start_args(&jp, &sp, task, steps, Some("mid_write")));
    assert!(
        !ok1,
        "mid_write must exit non-zero (injected crash). stderr={e1}"
    );
    let committed_after_c1 = count_completed_lines(&journal);
    let mem_after_c1 = count_task_memories(&store, task);
    eprintln!("[c1 mid_write] committed_lines={committed_after_c1} memories={mem_after_c1}");
    assert_eq!(
        committed_after_c1, 2,
        "mid_write should leave exactly 2 committed steps (0,1) + a torn line"
    );

    // ── Crash #2: mid_replay → re-enter replay, exit mid-way. ──
    // Tämä on ENSIMMÄINEN replayn-keskeyttävä kaatuminen.
    let (ok2, _o2, e2) = run(&bin, &start_args(&jp, &sp, task, steps, Some("mid_replay")));
    assert!(!ok2, "mid_replay must exit non-zero. stderr={e2}");
    let committed_after_c2 = count_completed_lines(&journal);
    let mem_after_c2 = count_task_memories(&store, task);
    eprintln!(
        "[c2 mid_replay] committed_lines={committed_after_c2} memories={mem_after_c2} stderr={}",
        e2.trim()
    );

    // ── Crash #3: mid_replay AGAIN → replay-of-replay. ──
    // Toinen replayn-keskeyttävä kaatuminen PERÄKKÄIN — "resume the resume".
    let (ok3, _o3, e3) = run(&bin, &start_args(&jp, &sp, task, steps, Some("mid_replay")));
    assert!(!ok3, "second mid_replay must exit non-zero. stderr={e3}");
    let committed_after_c3 = count_completed_lines(&journal);
    let mem_after_c3 = count_task_memories(&store, task);
    eprintln!(
        "[c3 mid_replay#2] committed_lines={committed_after_c3} memories={mem_after_c3} stderr={}",
        e3.trim()
    );

    // INVARIANTTI 1: toistuvat replay-kaatumiset eivät saa LISÄTÄ rivejä
    // journaliin (replay vain toistaa, ei kirjoita) — eikä turmella ehjiä.
    assert_eq!(
        committed_after_c3, committed_after_c1,
        "replay-of-replay crashes must NOT append/lose committed steps \
         (c1={committed_after_c1} c3={committed_after_c3})"
    );

    // INVARIANTTI 2: toistuvat replay-kaatumiset eivät saa kirjata muistoja
    // (mid_replay-polku ei persistoi) — ei kaksoiskirjausta.
    assert_eq!(
        mem_after_c3, mem_after_c1,
        "replay-of-replay must not write memories (c1={mem_after_c1} c3={mem_after_c3})"
    );

    // ── Final resume: täytyy jatkua puhtaasti. ──
    let resume_args = vec![
        "resume",
        "--journal",
        &jp,
        "--store",
        &sp,
        "--task",
        task,
        "--steps",
        steps,
        "--clock",
        CLOCK,
    ];
    let (okr, or, er) = run(&bin, &resume_args);
    assert!(okr, "final resume must succeed. stderr={er}");
    let report = result_json(&or);
    eprintln!("[resume] {report}");

    assert_eq!(
        report["resumed_clean"],
        serde_json::Value::Bool(true),
        "after replay-of-replay, final resume must reach the clean end state"
    );
    assert_eq!(
        report["was_replaying"],
        serde_json::Value::Bool(true),
        "journal had committed steps → resume must enter replay mode"
    );

    // INVARIANTTI 3 (side-effects exactly once): TASAN `steps` muistoa storessa,
    // ei enempää (kaksoiskirjaus) eikä vähempää (kadonnut askel). Luetaan store
    // raakana — ei luoteta daemonin laskuriin.
    let final_mem = count_task_memories(&store, task);
    eprintln!("[final] memories={final_mem} expected={steps}");
    assert_eq!(
        final_mem,
        steps.parse::<usize>().unwrap(),
        "side-effects exactly once: store must hold exactly {steps} task memories \
         after replay-of-replay (got {final_mem})"
    );

    // ── Väite PITI: replay-of-replay + ensimmäinen resume tuotti puhtaan
    //    lopputilan, sivuvaikutukset tasan kerran. Mekanismi: mid_replay vain
    //    toistaa (ei kirjoita journaliin eikä storeen), ja torn-rivi suodattuu
    //    replay-vektorista (`is_step`-filtteri + tolerantti viimeisen rivin parser).
    //
    // Aiemmin tämän alta löytyi seam (torn-rivi ei poistunut → tuore append
    // sulautui samalle riville → pysyvä korruptio). Se on nyt KORJATTU juurisyystä
    // (`FileJournal::open` heal-on-open typistää rivinvaihdottoman tyngän) ja
    // todistettu suljetuksi testissä `torn_write_then_resume_keeps_journal_readable_seam_closed`.
    let _ = std::fs::remove_dir_all(&dir);
}

/// REGRESSIO (seam suljettu): torn-write → resume EI enää turmele journalia.
///
/// **Aiempi seam (nyt korjattu, `familyclaw-durable/src/file.rs`):** `FileJournal`
/// sieti torn-viimeisen rivin luvussa mutta jätti sen levylle. Resume `append`:asi
/// tuoreen step-rivin SAMALLE fyysiselle riville (tyngästä puuttui `\n`) → syntyi
/// sisäkorruptio joka kaatoi jokaisen myöhemmän reopen/replayn.
///
/// **Korjaus (juurisyy):** `FileJournal::open` eheyttää (heal-on-open) tiedoston:
/// rivinvaihdoton, jäsentymätön tynkä typistetään pois ENNEN kirjoituskahvan
/// avaamista, joten jokainen append alkaa puhtaalta rivirajalta. Tynkä on aina
/// fsyncattamaton, sitoutumaton kirjoitus → sen hylkääminen on turvallista.
///
/// Tämä testi todistaa että hyökkäys EI enää riko väitettä:
/// 1. `mid_write` → torn line.
/// 2. 1. resume → puhdas (kuten ennenkin).
/// 3. journalissa EI ole sulautunutta riviä (ei kahta `step_id`:tä yhdellä rivillä).
/// 4. 2. resume → ONNISTUU edelleen (ennen: kuoli `CorruptEntry`:hin).
/// 5. resumen idempotenssi: sivuvaikutukset tasan kerran (3 muistoa, ei enempää).
#[test]
fn torn_write_then_resume_keeps_journal_readable_seam_closed() {
    let bin = daemon_bin();
    let dir = tempdir("seam-closed");
    let journal = dir.join("j.jsonl");
    let store = dir.join("s.json");
    let jp = journal.to_string_lossy().into_owned();
    let sp = store.to_string_lossy().into_owned();
    let task = "torn-seam";
    let steps = "3";

    // mid_write → torn last line.
    let (ok1, _o1, _e1) = run(&bin, &start_args(&jp, &sp, task, steps, Some("mid_write")));
    assert!(!ok1, "mid_write must crash");

    let resume_args = vec![
        "resume",
        "--journal",
        &jp,
        "--store",
        &sp,
        "--task",
        task,
        "--steps",
        steps,
        "--clock",
        CLOCK,
    ];

    // Ensimmäinen resume: ONNISTUU ja saavuttaa puhtaan lopputilan.
    let (okr, or, er) = run(&bin, &resume_args);
    assert!(okr, "first resume succeeds. stderr={er}");
    assert_eq!(
        result_json(&or)["resumed_clean"],
        serde_json::Value::Bool(true)
    );

    // KORJAUS-INVARIANTTI: yksikään fyysinen rivi ei saa sisältää kahta step_id:tä
    // — eli torn-tynkä + tuore rivi EIVÄT sulautuneet (heal-on-open typisti tyngän).
    let contents = std::fs::read_to_string(&journal).expect("read journal");
    let merged_garbage = contents
        .lines()
        .any(|l| l.matches("\"step_id\"").count() >= 2);
    assert!(
        !merged_garbage,
        "SEAM MUST BE CLOSED: no physical line may fuse two entries. journal=\n{contents}"
    );

    // TODISTE: toinen resume ONNISTUU edelleen — journal on yhä luettava.
    let (okr2, stdout_r2, er2) = run(&bin, &resume_args);
    assert!(
        okr2,
        "FIXED: second resume must succeed because the journal is no longer corrupt. stderr={er2}"
    );
    assert!(
        !er2.contains("corrupt journal entry"),
        "second resume must NOT hit CorruptEntry, got stderr: {er2}"
    );
    assert_eq!(
        result_json(&stdout_r2)["resumed_clean"],
        serde_json::Value::Bool(true),
        "second resume must also reach the clean end state"
    );

    // IDEMPOTENSSI: kahden resumen jälkeen TASAN `steps` muistoa — ei duplikaattia.
    let final_mem = count_task_memories(&store, task);
    assert_eq!(
        final_mem,
        steps.parse::<usize>().unwrap(),
        "side-effects exactly once across two resumes (got {final_mem})"
    );
    eprintln!("[SEAM CLOSED] both resumes clean, journal readable, {final_mem} memories");

    let _ = std::fs::remove_dir_all(&dir);
}

/// VARIANTTI: aloita PUHTAALLA täydellä journalilla, sitten kaada kesken
/// replayn kolmesti peräkkäin, sitten resume.
///
/// Tämä erottaa "replay-of-replay" -hyökkäyksen `mid_write`-jäämästä: tässä
/// journal on TÄYDELLINEN (3 askelta), ja jokainen `mid_replay` re-enteröi
/// täyden replayn ja kaatuu kesken. Resume EI saa kadottaa mitään eikä
/// tuottaa uusia muistoja (kaikki 3 jo storessa).
#[test]
fn full_journal_replay_crashes_thrice_then_resume_is_noop_clean() {
    let bin = daemon_bin();
    let dir = tempdir("full");
    let journal = dir.join("j.jsonl");
    let store = dir.join("s.json");
    let jp = journal.to_string_lossy().into_owned();
    let sp = store.to_string_lossy().into_owned();
    let task = "replay2-full";
    let steps = "4";

    // Puhdas täysi ajo → 4 askelta + 4 muistoa.
    let (ok0, _o0, e0) = run(&bin, &start_args(&jp, &sp, task, steps, None));
    assert!(ok0, "clean start must succeed. stderr={e0}");
    let committed0 = count_completed_lines(&journal);
    let mem0 = count_task_memories(&store, task);
    assert_eq!(committed0, 4, "clean start commits all 4 steps");
    assert_eq!(mem0, 4, "clean start persists 4 memories");

    // Kaadu kesken replayn KOLMESTI peräkkäin.
    for round in 1..=3 {
        let (ok, _o, e) = run(&bin, &start_args(&jp, &sp, task, steps, Some("mid_replay")));
        assert!(
            !ok,
            "mid_replay round {round} must exit non-zero. stderr={e}"
        );
        let committed = count_completed_lines(&journal);
        let mem = count_task_memories(&store, task);
        eprintln!(
            "[full mid_replay round {round}] committed={committed} mem={mem} stderr={}",
            e.trim()
        );
        assert_eq!(
            committed, committed0,
            "round {round}: replay crash must not change committed step count"
        );
        assert_eq!(
            mem, mem0,
            "round {round}: replay crash must not write/duplicate memories"
        );
    }

    // Resume: kaikki jo lokissa → puhdas, ei uusia muistoja.
    let resume_args = vec![
        "resume",
        "--journal",
        &jp,
        "--store",
        &sp,
        "--task",
        task,
        "--steps",
        steps,
        "--clock",
        CLOCK,
    ];
    let (okr, or, er) = run(&bin, &resume_args);
    assert!(okr, "resume must succeed. stderr={er}");
    let report = result_json(&or);
    eprintln!("[full resume] {report}");
    assert_eq!(report["resumed_clean"], serde_json::Value::Bool(true));
    let final_mem = count_task_memories(&store, task);
    assert_eq!(
        final_mem, 4,
        "side-effects exactly once after triple replay-crash (got {final_mem})"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
