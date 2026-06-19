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
//! ## Kaksi ikkunaa, molemmat todistettu prosessirajan yli
//! - **COMMITTED-ikkuna** (`crash` → `resume`): outbox on jo täysin kirjoitettu
//!   (intent + committed), vain agenttikerroksen journal-rivi puuttuu. Replay
//!   palauttaa **arvo-identtisen** lopputuloksen ajamatta sivuvaikutusta uudelleen
//!   → **exactly-once**.
//! - **INTENT-ONLY-ikkuna** (`crash_intent` → `resume_intent`): prosessi tapetaan
//!   `record_intent`:n JA sivuvaikutuksen jälkeen mutta ENNEN `record_committed`:ä.
//!   Replay näkee `InProgress` → `submit_task_idempotent` palauttaa `PolicyDenied`
//!   fail-closed, eikä sivuvaikutus aja uudelleen → **at-most-once**. Tämä on se
//!   aidosti vaarallinen ikkuna jonka GPT-5.5 nosti esiin; aiemmin se oli
//!   todistettu vain saman prosessin sisäisellä yksikkötestillä.
//!
//! ## Kolme väitettä
//! - **Vanha polku (`--mode old`, `submit_task_as` ilman outboxia):** bugi ON
//!   olemassa → sivuvaikutuslaskuri = 2 (double-fire), lopputulos ei identtinen.
//!   Tämä todistaa että testi PALJASTAA bugin (epäonnistuisi korjaamattomalla
//!   koodilla).
//! - **Uusi polku, COMMITTED-ikkuna (`--mode new`, `submit_task_idempotent` +
//!   kaatumiskestävä outbox):** bugi KORJATTU → sivuvaikutuslaskuri = 1 (tasan
//!   kerran), ja jatkettu lopputulos on **arvo-identtinen** kaatuneen kanssa
//!   (sama `task_id`).
//! - **Uusi polku, INTENT-ONLY-ikkuna:** kaatuminen intentin jälkeen ennen
//!   committedia → replay on `PolicyDenied` ja laskuri pysyy 1:ssä (at-most-once).

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

/// Ajaa harness-prosessin annetuilla ympäristömuuttujilla ja palauttaa
/// (`exit_code`, `exit_ok`, stdout, stderr).
///
/// Tarvitaan intent-only-kaatumiseen, joka aseistetaan
/// `FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT=1`:llä. `exit_code` palautetaan
/// erikseen jotta voidaan vaatia tasan 137 (SIGKILL-tyyli).
fn run_env(bin: &Path, args: &[&str], envs: &[(&str, &str)]) -> (Option<i32>, bool, String, String) {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn harness");
    (
        out.status.code(),
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

/// Ympäristömuuttuja joka aseistaa intent-only-kaatumiskoukun harness-binäärissä.
const CRASH_AFTER_INTENT_ENV: &str = "FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT";

/// Lukee outbox-journalin raakana (tyhjä jos tiedostoa ei ole).
fn read_outbox(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// **INTENT-ONLY-IKKUNA todistaa at-most-once fail-closed AIDON prosessirajan yli.**
///
/// Tämä sulkee sen kaveatin jonka GPT-5.5:n adversariaalinen katselmointi nosti
/// esiin: aiempi `crash`-vaihe poistui VASTA `record_committed`:n jälkeen (hyvänlaatuinen
/// committed-replay-kohta). Aidosti vaarallinen ikkuna on `record_intent`:n JA
/// sivuvaikutuksen jälkeen mutta ENNEN `record_committed`:ä — siellä replayn ON
/// palautettava `InProgress` → `PolicyDenied`, eikä sivuvaikutus saa laueta toiste.
///
/// Vaihe 1 (`crash_intent`, koukku aseistettu): `record_intent` levylle, sivuvaikutus
/// laukeaa (laskuri = 1), sitten prosessi abortoi `record_committed`:n alussa →
/// poistuu 137. Levyllä: intent-marker LÄSNÄ, committed-marker POISSA.
///
/// Vaihe 2 (`resume_intent`, sama avain): outbox-lookup näkee intentin ilman
/// committedia → `InProgress` → `submit_task_idempotent` palauttaa `PolicyDenied`.
/// Laskuri PYSYY 1:ssä (sivuvaikutus EI aja uudelleen) → at-most-once.
///
/// Testi epäonnistuisi jos järjestys olisi väärin: jos `record_intent` tulisi
/// sivuvaikutuksen JÄLKEEN, replay ei havaitsisi keskeneräisyyttä ja
/// double-firaisi (tai re-runaisi hiljaa) → tämä testi pyydystää sen.
#[test]
fn intent_window_crash_is_at_most_once() {
    let bin = harness_bin();
    let dir = tempdir("intent-window");
    let outbox = dir.join("outbox.jsonl");
    let counter = dir.join("counter.txt");
    let outcome = dir.join("outcome.json");
    let (ob, ct, oc) = (
        outbox.to_string_lossy().into_owned(),
        counter.to_string_lossy().into_owned(),
        outcome.to_string_lossy().into_owned(),
    );

    // Vaihe 1 (crash_intent): aseistettu koukku → abort record_committed:n alussa.
    let (code1, ok1, _o1, e1) = run_env(
        &bin,
        &phase_args("new", "crash_intent", &ob, &ct, &oc),
        &[(CRASH_AFTER_INTENT_ENV, "1")],
    );
    assert!(!ok1, "crash_intent phase must NOT exit success. stderr={e1}");
    assert_eq!(
        code1,
        Some(137),
        "crash_intent must exit 137 (SIGKILL-style) from the crash hook. stderr={e1}"
    );

    // Sivuvaikutus laukesi TASAN KERRAN ennen kaatumista (CountingExecutorin ulkoinen
    // levymerkki). Tämä todistaa että kaatuminen tapahtui sivuvaikutuksen JÄLKEEN.
    assert_eq!(
        read_counter(&counter),
        1,
        "side effect fired exactly once before the intent-only crash"
    );

    // Levyllä: intent-marker LÄSNÄ, committed-marker POISSA → todennettu
    // intent-only-tila aidon prosessirajan yli (ei in-process-simulaatio).
    let on_disk = read_outbox(&outbox);
    assert!(
        on_disk.contains("dispatch_intent"),
        "intent marker must be present on disk after intent-only crash. disk={on_disk:?}"
    );
    assert!(
        !on_disk.contains("dispatch_committed"),
        "committed marker must be ABSENT on disk (crash hit before record_committed). \
         disk={on_disk:?}"
    );

    // Vaihe 2 (resume_intent): tuore prosessi, sama avain → InProgress →
    // PolicyDenied fail-closed, eikä sivuvaikutus aja uudelleen.
    let (code2, ok2, o2, e2) = run_env(
        &bin,
        &phase_args("new", "resume_intent", &ob, &ct, &oc),
        // EI aseistusta resumessa — koukkua ei käytetä resume_intent-vaiheessa.
        &[],
    );
    assert!(
        ok2,
        "resume_intent phase must exit success (code={code2:?}). stderr={e2}"
    );
    let report = result_json(&o2);
    eprintln!("[intent-window resume] {report}");

    // AT-MOST-ONCE TODISTE 1: replay on PolicyDenied fail-closed (ei hiljainen re-run).
    assert_eq!(
        report["policy_denied"],
        serde_json::Value::Bool(true),
        "INTENT-ONLY replay must be PolicyDenied (fail-closed), not a silent re-run"
    );

    // AT-MOST-ONCE TODISTE 2: laskuri PYSYY 1:ssä — sivuvaikutus EI lauennut toiste.
    assert_eq!(
        report["side_effect_count"], 1,
        "INTENT-ONLY replay must NOT re-fire the side effect (at-most-once)"
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "disk counter confirms the side effect fired AT MOST once (1, never 2)"
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

// ============================================================================
// HYVÄKSYNTÄPOLKU (avaimet `approval-*`) — at-most-once aidon prosessirajan yli
// ============================================================================
//
// Tämä sulkee katselmoinnin löydöksen: aiempi cross-process-todiste kattoi vain
// `submit_task`-avaimet (`turn-*`), EI hyväksyntä-avaimia (`approval-{id}`).
// `ActionRuntime::approve` on idempotentti SAMAN dispatch-outboxin kautta
// (avain `approval-{id}`: lookup → record_intent → run_after_approval
// (sivuvaikutus) → record_committed → pending.remove), ja tässä todistetaan se
// AIDON SIGKILL:n (exit 137) yli — durable pending (Wire-vaihe) sallii tuoreen
// prosessin ladata SAMAN odottavan hyväksynnän levyltä ja uudelleenhyväksyä sen.

/// Ympäristömuuttuja joka aseistaa committed-ikkunan kaatumiskoukun harnessissa.
const CRASH_AFTER_COMMITTED_ENV: &str = "FAMILYCLAW_REDTEAM_CRASH_AFTER_COMMITTED";

/// Rakentaa hyväksyntäpolun `run`-vaiheen argumentit (`--pending` + `--task-queue`
/// lisätty `phase_args`:n päälle, koska durable pending = Wire-vaihe).
#[allow(clippy::too_many_arguments)]
fn approval_phase_args<'a>(
    phase: &'a str,
    outbox: &'a str,
    counter: &'a str,
    outcome: &'a str,
    pending: &'a str,
    task_queue: &'a str,
) -> Vec<&'a str> {
    vec![
        "run",
        "--mode",
        "new",
        "--phase",
        phase,
        "--outbox",
        outbox,
        "--counter",
        counter,
        "--outcome-out",
        outcome,
        "--pending",
        pending,
        "--task-queue",
        task_queue,
        "--clock",
        CLOCK,
    ]
}

/// **HYVÄKSYNTÄPOLKU, INTENT-ONLY-IKKUNA todistaa at-most-once fail-closed aidon
/// prosessirajan yli — avaimella `approval-{id}` (EI `turn-*`).**
///
/// Tämä on katselmoinnin löydöksen suora sulkeminen: aiempi cross-process-todiste
/// kattoi vain `submit_task`-avaimet. Nyt sama SIGKILL-todiste pätee
/// [`ActionRuntime::approve`]:n sivuvaikutus-ikkunaan.
///
/// Vaihe 1 (`approve_crash_intent`, koukku aseistettu): lähetä hyväksyntää vaativa
/// tehtävä → hyväksy → `run_after_approval` ajaa sivuvaikutuksen (laskuri = 1),
/// `record_intent` fsyncataan, prosessi abortoi `record_committed`:n alussa →
/// poistuu 137. Levyllä: intent-marker LÄSNÄ, committed-marker POISSA, odottava
/// hyväksyntä yhä durable-pinnalla.
///
/// Vaihe 2 (`approve_resume`, tuore prosessi): durable pending ladataan levyltä
/// (Wire), SAMA `ApprovalId` poimitaan ja uudelleenhyväksytään → outbox-lookup
/// näkee `InProgress` → `approve` palauttaa `PolicyDenied` fail-closed. Laskuri
/// PYSYY 1:ssä (sivuvaikutus EI aja uudelleen) → at-most-once.
///
/// Testi epäonnistuisi jos `approve` EI olisi idempotentti (outbox ohitettu):
/// re-approve ajaisi `run_after_approval`:n uudelleen → laskuri = 2. (Mutaatio-
/// todiste tehty erikseen poistamalla outbox-haara `approve`:sta.)
#[test]
#[allow(clippy::too_many_lines)]
fn approval_path_intent_crash_is_at_most_once() {
    let bin = harness_bin();
    let dir = tempdir("approval-intent");
    let outbox = dir.join("outbox.jsonl");
    let counter = dir.join("counter.txt");
    let outcome = dir.join("outcome.json");
    let pending = dir.join("pending.jsonl");
    let task_queue = dir.join("tasks.jsonl");
    let (ob, ct, oc, pd, tq) = (
        outbox.to_string_lossy().into_owned(),
        counter.to_string_lossy().into_owned(),
        outcome.to_string_lossy().into_owned(),
        pending.to_string_lossy().into_owned(),
        task_queue.to_string_lossy().into_owned(),
    );

    // Vaihe 1 (approve_crash_intent): aseistettu intent-koukku → abort
    // record_committed:n alussa hyväksynnän jälkeen.
    let (code1, ok1, _o1, e1) = run_env(
        &bin,
        &approval_phase_args("approve_crash_intent", &ob, &ct, &oc, &pd, &tq),
        &[(CRASH_AFTER_INTENT_ENV, "1")],
    );
    assert!(
        !ok1,
        "approve_crash_intent phase must NOT exit success. stderr={e1}"
    );
    assert_eq!(
        code1,
        Some(137),
        "approve_crash_intent must exit 137 (SIGKILL-style) from the crash hook. stderr={e1}"
    );

    // Sivuvaikutus laukesi TASAN KERRAN ennen kaatumista (approve ajoi
    // run_after_approval:n, joka bumppasi laskurin).
    assert_eq!(
        read_counter(&counter),
        1,
        "side effect fired exactly once before the approval intent-only crash"
    );

    // Levyllä: intent-marker LÄSNÄ avaimella approval-*, committed-marker POISSA.
    let on_disk = read_outbox(&outbox);
    assert!(
        on_disk.contains("dispatch_intent"),
        "intent marker must be present on disk after approval intent-only crash. disk={on_disk:?}"
    );
    assert!(
        on_disk.contains("approval-"),
        "outbox key must be approval-* (NOT turn-*) on the approval path. disk={on_disk:?}"
    );
    assert!(
        !on_disk.contains("dispatch_committed"),
        "committed marker must be ABSENT on disk (crash hit before record_committed). \
         disk={on_disk:?}"
    );
    // Odottava hyväksyntä SÄILYI durable-pinnalla (Wire) → resume voi ladata sen.
    let pending_on_disk = std::fs::read_to_string(&pending).unwrap_or_default();
    assert!(
        pending_on_disk.contains("pending_approval_put"),
        "durable pending must still hold the approval after the intent-only crash \
         (Wire phase). pending={pending_on_disk:?}"
    );

    // Vaihe 2 (approve_resume): tuore prosessi, lataa sama ApprovalId levyltä,
    // uudelleenhyväksyy → InProgress → PolicyDenied fail-closed.
    let (code2, ok2, o2, e2) = run_env(
        &bin,
        &approval_phase_args("approve_resume", &ob, &ct, &oc, &pd, &tq),
        // EI aseistusta resumessa — koukkua ei käytetä.
        &[],
    );
    assert!(
        ok2,
        "approve_resume phase must exit success (code={code2:?}). stderr={e2}"
    );
    let report = result_json(&o2);
    eprintln!("[approval-intent resume] {report}");

    // AT-MOST-ONCE TODISTE 1: uudelleenhyväksyntä on PolicyDenied fail-closed.
    assert_eq!(
        report["policy_denied"],
        serde_json::Value::Bool(true),
        "APPROVAL intent-only replay must be PolicyDenied (fail-closed), not a silent re-run"
    );

    // AT-MOST-ONCE TODISTE 2: laskuri PYSYY 1:ssä — sivuvaikutus EI lauennut toiste.
    assert_eq!(
        report["side_effect_count"], 1,
        "APPROVAL intent-only replay must NOT re-fire the side effect (at-most-once)"
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "disk counter confirms the approval side effect fired AT MOST once (1, never 2)"
    );

    // Wire-vaiheen todiste: tuore prosessi todella latasi SAMAN hyväksynnän levyltä.
    assert!(
        report["reloaded_approval_id"].is_string(),
        "fresh process must have reloaded the durable pending approval id"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **HYVÄKSYNTÄPOLKU, COMMITTED-IKKUNA todistaa arvo-identtisen replayn aidon
/// prosessirajan yli — avaimella `approval-{id}`.**
///
/// Vaihe 1 (`approve_crash_committed`, koukku aseistettu): hyväksy → sivuvaikutus
/// laukeaa (laskuri = 1), `record_committed` fsyncataan, prosessi abortoi VASTA
/// committedin jälkeen mutta ENNEN `pending.remove`:a → poistuu 137. Levyllä:
/// intent + committed LÄSNÄ.
///
/// Vaihe 2 (`approve_resume`): tuore prosessi lataa saman `ApprovalId`:n →
/// outbox-lookup näkee `Committed` → palauttaa arvo-identtisen `SubmitOutcome`:n
/// (sama `task_id`, status Done) ajamatta sivuvaikutusta uudelleen. Laskuri = 1.
#[test]
#[allow(clippy::too_many_lines)]
fn approval_path_committed_crash_is_value_identical() {
    let bin = harness_bin();
    let dir = tempdir("approval-committed");
    let outbox = dir.join("outbox.jsonl");
    let counter = dir.join("counter.txt");
    let outcome = dir.join("outcome.json");
    let pending = dir.join("pending.jsonl");
    let task_queue = dir.join("tasks.jsonl");
    let (ob, ct, oc, pd, tq) = (
        outbox.to_string_lossy().into_owned(),
        counter.to_string_lossy().into_owned(),
        outcome.to_string_lossy().into_owned(),
        pending.to_string_lossy().into_owned(),
        task_queue.to_string_lossy().into_owned(),
    );

    // Vaihe 1 (approve_crash_committed): koukku abortoi VASTA committedin jälkeen.
    let (code1, ok1, _o1, e1) = run_env(
        &bin,
        &approval_phase_args("approve_crash_committed", &ob, &ct, &oc, &pd, &tq),
        &[(CRASH_AFTER_COMMITTED_ENV, "1")],
    );
    assert!(
        !ok1,
        "approve_crash_committed phase must NOT exit success. stderr={e1}"
    );
    assert_eq!(
        code1,
        Some(137),
        "approve_crash_committed must exit 137 from the crash hook. stderr={e1}"
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "side effect fired exactly once before the committed-window crash"
    );

    // Levyllä: intent + committed LÄSNÄ, avain approval-*.
    let on_disk = read_outbox(&outbox);
    assert!(
        on_disk.contains("dispatch_intent") && on_disk.contains("dispatch_committed"),
        "both intent and committed markers must be on disk (committed window). disk={on_disk:?}"
    );
    assert!(
        on_disk.contains("approval-"),
        "outbox key must be approval-* on the approval path. disk={on_disk:?}"
    );

    // Vaihe 2 (approve_resume): sama ApprovalId → Committed → arvo-identtinen.
    let (code2, ok2, o2, e2) = run_env(
        &bin,
        &approval_phase_args("approve_resume", &ob, &ct, &oc, &pd, &tq),
        &[],
    );
    assert!(
        ok2,
        "approve_resume phase must exit success (code={code2:?}). stderr={e2}"
    );
    let report = result_json(&o2);
    eprintln!("[approval-committed resume] {report}");

    // KORJAUKSEN TODISTE 1 (arvo-identtisyys): re-approve palauttaa saman task_id:n.
    assert_eq!(
        report["value_identical"],
        serde_json::Value::Bool(true),
        "APPROVAL committed replay must return the value-identical SubmitOutcome (same task_id)"
    );
    assert_eq!(
        report["policy_denied"],
        serde_json::Value::Bool(false),
        "APPROVAL committed replay must NOT be denied (it returns the committed outcome)"
    );

    // KORJAUKSEN TODISTE 2 (at-most-once): laskuri PYSYY 1:ssä.
    assert_eq!(
        report["side_effect_count"], 1,
        "APPROVAL committed replay must NOT re-fire the side effect (exactly-once dispatch)"
    );
    assert_eq!(
        read_counter(&counter),
        1,
        "disk counter confirms the approval side effect fired exactly once"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
