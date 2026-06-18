//! RED-TEAM: exactly-once-lähetys SIGKILL:n yli kesken lähetyksen.
//!
//! Hyökkäys (GPT-5.5:n löytö): agenttikerros ajaa [`ActionRuntime`]:n
//! sivuvaikutuksen (`submit_task`) ENNEN kuin se journaloi dispatch-rivin. Jos
//! prosessi tapetaan (SIGKILL) siinä ikkunassa — sivuvaikutus on jo tapahtunut,
//! journal-riviä ei ole — replay/restart luulee askelta ajamattomaksi ja **ajaa
//! sivuvaikutuksen uudelleen** (kaksoislaukaisu).
//!
//! Tämä ajetaan **aidon prosessirajan yli**: erillinen `dispatch_redteam`-prosessi
//! poistuu exit-koodilla 137 (SIGKILL-tyyli) juuri siinä ikkunassa, ja toinen
//! prosessi yrittää jatkaa. Kello injektoidaan → deterministinen.
//!
//! ## Kaksi väitettä
//! - **Vanha polku (`--mode old`, `submit_task_as` ilman outboxia):** bugi ON
//!   olemassa → sivuvaikutuslaskuri = 2 (double-fire), lopputulos ei identtinen.
//!   Tämä todistaa että testi PALJASTAA bugin (epäonnistuisi korjaamattomalla
//!   koodilla).
//! - **Uusi polku (`--mode new`, `submit_task_idempotent` + kaatumiskestävä
//!   outbox):** bugi KORJATTU → sivuvaikutuslaskuri = 1 (tasan kerran), ja
//!   jatkettu lopputulos on **arvo-identtinen** kaatuneen kanssa (sama `task_id`).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Paikantaa `dispatch_redteam`-binäärin saman profiilin kansiosta.
fn harness_bin() -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let deps = exe.parent().expect("deps dir");
    let profile_dir = deps.parent().expect("profile dir");
    let mut bin = profile_dir.join("dispatch_redteam");
    if cfg!(windows) {
        bin.set_extension("exe");
    }
    assert!(
        bin.exists(),
        "dispatch_redteam binary not found at {} — build it first \
         (cargo build -p familyclaw-actions --bin dispatch_redteam)",
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
        "familyclaw-redteam-dispatch-{}-{}-{tag}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    std::fs::create_dir_all(&dir).expect("create tempdir");
    dir
}

/// Ajaa harness-prosessin ja palauttaa (`exit_ok`, stdout, stderr).
fn run(bin: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(bin).args(args).output().expect("spawn harness");
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

/// Rakentaa yhden `run`-vaiheen argumentit.
fn phase_args<'a>(
    mode: &'a str,
    phase: &'a str,
    outbox: &'a str,
    counter: &'a str,
    outcome: &'a str,
) -> Vec<&'a str> {
    vec![
        "run",
        "--mode",
        mode,
        "--phase",
        phase,
        "--outbox",
        outbox,
        "--counter",
        counter,
        "--outcome-out",
        outcome,
        "--clock",
        CLOCK,
    ]
}

/// Lukee sivuvaikutuslaskurin raakana (0 jos tiedostoa ei ole).
fn read_counter(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// **VANHA polku todistaa bugin:** `submit_task_as` ilman outboxia → kaatuminen
/// kesken lähetyksen + re-drive AJAA SIVUVAIKUTUKSEN UUDELLEEN (double-fire).
///
/// Tämä testi VARMISTAA että red-team-harness oikeasti paljastaa bugin: jos
/// korjaus poistettaisiin (palaisi `submit_task_as`:iin), uuden polun testi
/// epäonnistuisi — tässä todistetaan että vanha koodi tuottaa laskurin 2.
#[test]
fn old_path_double_fires_side_effect_across_crash() {
    let bin = harness_bin();
    let dir = tempdir("old");
    let outbox = dir.join("outbox.jsonl");
    let counter = dir.join("counter.txt");
    let outcome = dir.join("outcome.json");
    let (ob, ct, oc) = (
        outbox.to_string_lossy().into_owned(),
        counter.to_string_lossy().into_owned(),
        outcome.to_string_lossy().into_owned(),
    );

    // Vaihe 1 (crash): aja lähetys (sivuvaikutus +1), poistu 137 ennen journalointia.
    let (ok1, _o1, e1) = run(&bin, &phase_args("old", "crash", &ob, &ct, &oc));
    assert!(!ok1, "crash phase must exit non-zero. stderr={e1}");
    assert_eq!(read_counter(&counter), 1, "side effect ran once before crash");

    // Vaihe 2 (resume): re-drive SAMA lähetys — vanhalla polulla ei idempotenssia.
    let (ok2, o2, e2) = run(&bin, &phase_args("old", "resume", &ob, &ct, &oc));
    assert!(ok2, "resume phase must succeed. stderr={e2}");
    let report = result_json(&o2);
    eprintln!("[old resume] {report}");

    // BUGIN TODISTE: sivuvaikutus laukesi KAHDESTI (kaatumisikkuna + re-drive).
    assert_eq!(
        report["side_effect_count"], 2,
        "OLD path: side effect double-fires across the crash window (THIS is the bug)"
    );
    assert_eq!(read_counter(&counter), 2, "disk counter confirms double-fire");
    // Eikä lopputulos ole identtinen (uusi satunnais-task_id re-drivessä).
    assert_eq!(
        report["value_identical"],
        serde_json::Value::Bool(false),
        "OLD path re-run produces a DIFFERENT task_id (not value-identical)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **UUSI polku todistaa korjauksen:** `submit_task_idempotent` + kaatumiskestävä
/// outbox → kaatuminen kesken lähetyksen + re-drive EI aja sivuvaikutusta
/// uudelleen, ja jatkettu lopputulos on arvo-identtinen kaatuneen kanssa.
#[test]
fn new_path_side_effect_exactly_once_and_value_identical() {
    let bin = harness_bin();
    let dir = tempdir("new");
    let outbox = dir.join("outbox.jsonl");
    let counter = dir.join("counter.txt");
    let outcome = dir.join("outcome.json");
    let (ob, ct, oc) = (
        outbox.to_string_lossy().into_owned(),
        counter.to_string_lossy().into_owned(),
        outcome.to_string_lossy().into_owned(),
    );

    // Vaihe 1 (crash): aja idempotentti lähetys (intent + sivuvaikutus + committed),
    // poistu 137 ennen kuin agentti ehtisi journaloida dispatch-rivin.
    let (ok1, _o1, e1) = run(&bin, &phase_args("new", "crash", &ob, &ct, &oc));
    assert!(!ok1, "crash phase must exit non-zero. stderr={e1}");
    assert_eq!(read_counter(&counter), 1, "side effect ran once before crash");

    // Vaihe 2 (resume): re-drive SAMA lähetys samalla idempotenssi-avaimella.
    // Outbox palauttaa committed-lopputuloksen ajamatta sivuvaikutusta uudelleen.
    let (ok2, o2, e2) = run(&bin, &phase_args("new", "resume", &ob, &ct, &oc));
    assert!(ok2, "resume phase must succeed. stderr={e2}");
    let report = result_json(&o2);
    eprintln!("[new resume] {report}");

    // KORJAUKSEN TODISTE 1 (exactly-once): sivuvaikutus laukesi TASAN KERRAN.
    assert_eq!(
        report["side_effect_count"], 1,
        "NEW path: side effect must NOT re-fire after the crash (exactly-once)"
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "disk counter confirms side effect ran exactly once"
    );

    // KORJAUKSEN TODISTE 2 (arvo-identtisyys): jatkettu lopputulos = kaatunut.
    assert_eq!(
        report["value_identical"],
        serde_json::Value::Bool(true),
        "NEW path resume must return the value-identical SubmitOutcome (same task_id)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Erottava väite: vanha ja uusi polku eroavat TÄSMÄLLEEN tässä — vanha
/// double-firaa (2), uusi ei (1). Tämä on koko korjauksen ydin yhdellä rivillä.
#[test]
fn old_double_fires_but_new_does_not() {
    let bin = harness_bin();

    // OLD → 2
    let dir_old = tempdir("contrast-old");
    let ob = dir_old.join("o.jsonl").to_string_lossy().into_owned();
    let ct = dir_old.join("c.txt").to_string_lossy().into_owned();
    let oc = dir_old.join("oc.json").to_string_lossy().into_owned();
    let _ = run(&bin, &phase_args("old", "crash", &ob, &ct, &oc));
    let _ = run(&bin, &phase_args("old", "resume", &ob, &ct, &oc));
    let old_count = read_counter(Path::new(&ct));

    // NEW → 1
    let dir_new = tempdir("contrast-new");
    let ob = dir_new.join("o.jsonl").to_string_lossy().into_owned();
    let ct = dir_new.join("c.txt").to_string_lossy().into_owned();
    let oc = dir_new.join("oc.json").to_string_lossy().into_owned();
    let _ = run(&bin, &phase_args("new", "crash", &ob, &ct, &oc));
    let _ = run(&bin, &phase_args("new", "resume", &ob, &ct, &oc));
    let new_count = read_counter(Path::new(&ct));

    assert_eq!(old_count, 2, "old path double-fires");
    assert_eq!(new_count, 1, "new path fires exactly once");
    assert!(
        old_count > new_count,
        "the fix strictly reduces side-effect count under the crash window \
         (old={old_count}, new={new_count})"
    );

    let _ = std::fs::remove_dir_all(&dir_old);
    let _ = std::fs::remove_dir_all(&dir_new);
}
