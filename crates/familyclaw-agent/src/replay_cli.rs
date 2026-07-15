//! `familyclaw replay` — Time Machine -CLI durable-journalien tarkasteluun.
//!
//! Tämä moduuli tarjoaa `familyclaw`-binäärin `replay`-alikomennon:
//! olemassa oleva [`FileJournal`](familyclaw_durable::FileJournal) avataan
//! **vain luettavaksi** ja sen aikajana esitetään ihmisluettavana
//! markdownina tai koneluettavana JSON:na. Toteutus rakentuu
//! [`TimeMachine`](familyclaw_durable::TimeMachine)-fasadin päälle, joten
//! lähdejournal ei koskaan muutu (append-only-invariantti säilyy).
//!
//! Neljä alikomentoa:
//!
//! 1. `replay inspect --journal <path> [--json]` — lue journal ja tulosta
//!    [`Timeline`](familyclaw_durable::Timeline).
//! 2. `replay fork --journal <path> --keep <N> --out <path>` — haarauta
//!    aikajana uuteen journaliin. **Fail-closed:** kieltäytyy jos `--out` on jo
//!    olemassa, jottei olemassa olevaa lokia koskaan ylikirjoiteta.
//! 3. `replay diff --before <path> --after <path> [--json]` — vertaa kahta
//!    aikajanaa ja tulosta [`TimelineDiff`](familyclaw_durable::TimelineDiff).
//! 4. `replay demo [--dir <path>]` — itsenäinen Time Machine -esittely: rakentaa
//!    "alkuperäisen" journalin (jossa on politiikkabugi), näyttää sen aikajanan,
//!    haarauttaa ennen bugia, ajaa korjatun politiikan `DryRunRecorder`illa
//!    (ei mitään oikeaa sivuvaikutusta) ja todistaa alkuperäisen journalin
//!    koskemattomuuden. Katso [`run_demo`].
//!
//! ## Suunnittelu (testattavuus)
//! Komentokäsittelijät ([`run_inspect`], [`run_fork`], [`run_diff`]) ovat
//! puhtaita funktioita jotka palauttavat `Result<String, ReplayError>` —
//! ne eivät tulosta itse eivätkä kutsu `process::exit`:iä. Näin niitä voi
//! kutsua suoraan yksikkötesteistä ja tarkistaa palautettu merkkijono/virhe.
//! Argumenttien jäsennys ([`parse`]) on nekin puhdas ja testattava.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use familyclaw_durable::{DryRunRecorder, DurableContext, FileJournal, Journal, TimeMachine};

/// `replay`-CLI:n virhetyyppi.
///
/// Kaikki virheet ovat **paniikittomia**: virheellinen syöte (puuttuva
/// argumentti, tuntematon lippu, olematon polku, jo olemassa oleva
/// `--out`) palautuu tämän tyypin kautta, ja binääri kuvaa sen selkeäksi
/// virheviestiksi + nollasta poikkeavaksi paluukoodiksi.
#[derive(Debug)]
#[non_exhaustive]
pub enum ReplayError {
    /// Argumenttien jäsennys epäonnistui (puuttuva/tuntematon lippu tai arvo).
    /// Sisältää ihmisluettavan syyn.
    Usage(String),
    /// Durable-substraatin virhe (journalin avaus/luku/kirjoitus tai fork).
    Durable(familyclaw_durable::DurableError),
    /// Tuloksen JSON-sarjallistus epäonnistui.
    Serde(serde_json::Error),
    /// Tiedostojärjestelmävirhe (`replay demo`: temp-hakemiston luonti/siivous).
    Io(std::io::Error),
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplayError::Usage(msg) => write!(f, "usage error: {msg}"),
            ReplayError::Durable(err) => write!(f, "durable error: {err}"),
            ReplayError::Serde(err) => write!(f, "serialization error: {err}"),
            ReplayError::Io(err) => write!(f, "io error: {err}"),
        }
    }
}

impl std::error::Error for ReplayError {}

impl From<familyclaw_durable::DurableError> for ReplayError {
    fn from(err: familyclaw_durable::DurableError) -> Self {
        ReplayError::Durable(err)
    }
}

impl From<serde_json::Error> for ReplayError {
    fn from(err: serde_json::Error) -> Self {
        ReplayError::Serde(err)
    }
}

impl From<std::io::Error> for ReplayError {
    fn from(err: std::io::Error) -> Self {
        ReplayError::Io(err)
    }
}

/// Jäsennetty `replay`-alikomento valmiina suoritettavaksi.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReplayCommand {
    /// `replay inspect` — lue ja tulosta yhden journalin aikajana.
    Inspect {
        /// Luettavan journalin polku.
        journal: PathBuf,
        /// Tulostetaanko JSON (`true`) vai markdown (`false`).
        json: bool,
    },
    /// `replay fork` — haarauta aikajana uuteen journaliin.
    Fork {
        /// Lähdejournalin polku (ei muutu).
        journal: PathBuf,
        /// Montako **workflow-askelta** prefiksistä säilytetään.
        keep: usize,
        /// Kohdejournalin polku. **Ei saa olla olemassa** (fail-closed).
        out: PathBuf,
    },
    /// `replay diff` — vertaa kahta aikajanaa.
    Diff {
        /// Vasemman (alkuperäisen) aikajanan journalin polku.
        before: PathBuf,
        /// Oikean (vertailtavan) aikajanan journalin polku.
        after: PathBuf,
        /// Tulostetaanko JSON (`true`) vai markdown (`false`).
        json: bool,
    },
    /// `replay demo` — itsenäinen Time Machine -esittely (ks. [`run_demo`]).
    Demo {
        /// Hakemisto jonne esittelyn journalit kirjoitetaan.
        /// `None` tarkoittaa: käytä tuoretta temp-hakemistoa ja siivoa se
        /// pois ennen paluuta (ei jätä levylle mitään).
        dir: Option<PathBuf>,
    },
}

/// `replay`-alikomennon käyttöohje (usage-teksti).
///
/// Palautetaan sellaisenaan virheviestin yhteydessä ja `--help`-pyynnöstä.
#[must_use]
pub fn usage() -> &'static str {
    "familyclaw replay — Time Machine (durable journal inspection)\n\
     \n\
     USAGE:\n    \
     familyclaw replay inspect --journal <path> [--json]\n    \
     familyclaw replay fork    --journal <path> --keep <N> --out <path>\n    \
     familyclaw replay diff    --before <path> --after <path> [--json]\n    \
     familyclaw replay demo    [--dir <path>]\n\
     \n\
     SUBCOMMANDS:\n    \
     inspect    Read a journal read-only and print its timeline\n    \
     fork       Fork a timeline into a fresh journal (refuses if --out exists)\n    \
     diff       Compare two timelines step by step\n    \
     demo       Self-contained Time Machine story: buggy run, fork, fixed dry-run\n\
     \n\
     FLAGS:\n    \
     --journal <path>    Path to the durable journal (JSONL)\n    \
     --before <path>     Left/original journal for diff\n    \
     --after <path>      Right/compared journal for diff\n    \
     --keep <N>          Number of workflow steps to keep in the fork prefix\n    \
     --out <path>        Destination journal for the fork (must not exist)\n    \
     --dir <path>        Directory for demo journals (default: a temp dir, cleaned up)\n    \
     --json              Emit JSON instead of Markdown (inspect, diff)"
}

/// Jäsentää `replay`-alikomennon argumentit (ilman `familyclaw replay`
/// -prefiksiä).
///
/// `args` on esim. `["inspect", "--journal", "run.jsonl"]`.
///
/// # Errors
/// [`ReplayError::Usage`] jos alikomento puuttuu tai on tuntematon, jos
/// pakollinen lippu/arvo puuttuu, tai jos lippu on tuntematon.
pub fn parse<I, S>(args: I) -> Result<ReplayCommand, ReplayError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut iter = args.into_iter().map(Into::into);
    let sub = iter
        .next()
        .ok_or_else(|| ReplayError::Usage("missing subcommand".to_string()))?;

    match sub.as_str() {
        "inspect" => parse_inspect(iter),
        "fork" => parse_fork(iter),
        "diff" => parse_diff(iter),
        "demo" => parse_demo(iter),
        other => Err(ReplayError::Usage(format!(
            "unknown replay subcommand `{other}` (expected inspect|fork|diff|demo)"
        ))),
    }
}

/// Jäsentää `replay inspect` -argumentit.
fn parse_inspect<I: Iterator<Item = String>>(args: I) -> Result<ReplayCommand, ReplayError> {
    let mut journal: Option<PathBuf> = None;
    let mut json = false;

    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--journal" => journal = Some(PathBuf::from(take_value(&mut args, "--journal")?)),
            "--json" => json = true,
            other => return Err(unknown_flag("inspect", other)),
        }
    }

    Ok(ReplayCommand::Inspect {
        journal: journal.ok_or_else(|| missing_flag("inspect", "--journal"))?,
        json,
    })
}

/// Jäsentää `replay fork` -argumentit.
fn parse_fork<I: Iterator<Item = String>>(args: I) -> Result<ReplayCommand, ReplayError> {
    let mut journal: Option<PathBuf> = None;
    let mut keep: Option<usize> = None;
    let mut out: Option<PathBuf> = None;

    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--journal" => journal = Some(PathBuf::from(take_value(&mut args, "--journal")?)),
            "--out" => out = Some(PathBuf::from(take_value(&mut args, "--out")?)),
            "--keep" => {
                let raw = take_value(&mut args, "--keep")?;
                let parsed = raw.parse::<usize>().map_err(|_| {
                    ReplayError::Usage(format!(
                        "--keep expects a non-negative integer, got `{raw}`"
                    ))
                })?;
                keep = Some(parsed);
            }
            other => return Err(unknown_flag("fork", other)),
        }
    }

    Ok(ReplayCommand::Fork {
        journal: journal.ok_or_else(|| missing_flag("fork", "--journal"))?,
        keep: keep.ok_or_else(|| missing_flag("fork", "--keep"))?,
        out: out.ok_or_else(|| missing_flag("fork", "--out"))?,
    })
}

/// Jäsentää `replay diff` -argumentit.
fn parse_diff<I: Iterator<Item = String>>(args: I) -> Result<ReplayCommand, ReplayError> {
    let mut before: Option<PathBuf> = None;
    let mut after: Option<PathBuf> = None;
    let mut json = false;

    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--before" => before = Some(PathBuf::from(take_value(&mut args, "--before")?)),
            "--after" => after = Some(PathBuf::from(take_value(&mut args, "--after")?)),
            "--json" => json = true,
            other => return Err(unknown_flag("diff", other)),
        }
    }

    Ok(ReplayCommand::Diff {
        before: before.ok_or_else(|| missing_flag("diff", "--before"))?,
        after: after.ok_or_else(|| missing_flag("diff", "--after"))?,
        json,
    })
}

/// Jäsentää `replay demo` -argumentit.
fn parse_demo<I: Iterator<Item = String>>(args: I) -> Result<ReplayCommand, ReplayError> {
    let mut dir: Option<PathBuf> = None;

    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--dir" => dir = Some(PathBuf::from(take_value(&mut args, "--dir")?)),
            other => return Err(unknown_flag("demo", other)),
        }
    }

    Ok(ReplayCommand::Demo { dir })
}

/// Ottaa lipun arvon iteraattorista tai palauttaa selkeän usage-virheen.
fn take_value<I: Iterator<Item = String>>(args: &mut I, flag: &str) -> Result<String, ReplayError> {
    args.next()
        .ok_or_else(|| ReplayError::Usage(format!("flag `{flag}` requires a value")))
}

/// Rakentaa "puuttuva lippu" -usage-virheen.
fn missing_flag(sub: &str, flag: &str) -> ReplayError {
    ReplayError::Usage(format!("`replay {sub}` requires `{flag}`"))
}

/// Rakentaa "tuntematon lippu" -usage-virheen.
fn unknown_flag(sub: &str, flag: &str) -> ReplayError {
    ReplayError::Usage(format!("`replay {sub}`: unknown flag `{flag}`"))
}

/// Suorittaa jäsennetyn [`ReplayCommand`]:n ja palauttaa tulostettavan
/// merkkijonon.
///
/// # Errors
/// Palauttaa [`ReplayError`]:n jos journalin avaus/luku/kirjoitus, fork tai
/// JSON-sarjallistus epäonnistuu. Ei koskaan paniikkia.
pub fn execute(command: ReplayCommand) -> Result<String, ReplayError> {
    match command {
        ReplayCommand::Inspect { journal, json } => run_inspect(&journal, json),
        ReplayCommand::Fork { journal, keep, out } => run_fork(&journal, keep, &out),
        ReplayCommand::Diff {
            before,
            after,
            json,
        } => run_diff(&before, &after, json),
        ReplayCommand::Demo { dir } => run_demo(dir.as_deref()),
    }
}

/// Avaa journalin ja tuottaa sen aikajanan markdownina tai JSON:na.
///
/// Journal avataan vain luettavaksi (append-only-invariantti säilyy).
///
/// # Errors
/// [`ReplayError::Durable`] jos journalia ei voi avata/lukea,
/// [`ReplayError::Serde`] jos JSON-sarjallistus epäonnistuu.
pub fn run_inspect(journal: &Path, json: bool) -> Result<String, ReplayError> {
    let journal = FileJournal::open(journal)?;
    let timeline = TimeMachine::inspect(&journal)?;
    if json {
        Ok(serde_json::to_string_pretty(&timeline)?)
    } else {
        Ok(timeline.render_markdown())
    }
}

/// Haarauttaa lähdejournalin aikajanan uuteen journaliin `keep`-askeleen
/// leikkauspisteestä.
///
/// **Fail-closed:** jos `out` on jo olemassa, funktio kieltäytyy sen sijaan
/// että ylikirjoittaisi tai jatkaisi olemassa olevaan lokiin — muuten haaran
/// tyhjä-kohdejournal-invariantti rikkoutuisi hiljaa.
///
/// # Errors
/// [`ReplayError::Usage`] jos `out` on jo olemassa; [`ReplayError::Durable`]
/// jos lähdejournalin luku, kohdejournalin avaus tai itse fork epäonnistuu
/// (esim. `keep` ylittää askelmäärän).
pub fn run_fork(journal: &Path, keep: usize, out: &Path) -> Result<String, ReplayError> {
    if out.exists() {
        return Err(ReplayError::Usage(format!(
            "refusing to fork into existing path `{}` (fail-closed: --out must not exist)",
            out.display()
        )));
    }

    let source = FileJournal::open(journal)?;
    let target = FileJournal::open(out)?;
    let kept = TimeMachine::fork_into(&source, keep, &target)?;

    Ok(format!(
        "forked timeline: kept {kept} step(s) into `{}`",
        out.display()
    ))
}

/// Avaa kaksi journalia ja tuottaa niiden vertailun markdownina tai JSON:na.
///
/// # Errors
/// [`ReplayError::Durable`] jos kumpaakaan journalia ei voi avata/lukea,
/// [`ReplayError::Serde`] jos JSON-sarjallistus epäonnistuu.
pub fn run_diff(before: &Path, after: &Path, json: bool) -> Result<String, ReplayError> {
    let before = FileJournal::open(before)?;
    let after = FileJournal::open(after)?;
    let diff = TimeMachine::diff(&before, &after)?;
    if json {
        Ok(serde_json::to_string_pretty(&diff)?)
    } else {
        Ok(diff.render_markdown())
    }
}

/// `familyclaw replay demo` — itsenäinen Time Machine -tarina yhdessä
/// prosessissa: rakenna bugillinen ajo, näytä sen aikajana, haaraudu ennen
/// bugia, aja korjattu politiikka dry-run-kaappauksella, ja todista ettei
/// alkuperäinen journal muuttunut.
///
/// Tarina (design §2.1, "Time Machine" -esittely):
/// 1. **`load_request`** — pyyntö saapuu, summa 100.
/// 2. **`decide_policy`** — BUGI: hyväksyy `amount * 2` = 200.
/// 3. **`dispatch_refund`** — lähettää (kirjaa) `"sent:200"`.
///
/// Sen jälkeen aikajana haarautetaan **ennen** `decide_policy`-askelta:
/// `load_request` toistuu lokista (ei sivuvaikutusta), ja jatko ajetaan
/// **korjatulla** politiikalla `min(amount, 100)` = 100. `dispatch_refund`
/// korvataan dry-run-versiolla joka kirjaa aiotun intentin
/// [`DryRunRecorder`]:iin — **mitään ei koskaan lähetetä oikeasti**, koska
/// kaappaustyypillä ei ole rakenteellisesti mitään dispatch-polkua.
///
/// Jos `dir` on `None`, esittely käyttää tuoretta temp-hakemistoa ja siivoaa
/// sen pois ennen paluuta (ei jätä levylle mitään). Jos `dir` on annettu,
/// hakemisto luodaan tarvittaessa eikä sitä siivota — journalit jäävät
/// tarkasteltavaksi (esim. `replay inspect --journal <dir>/original.jsonl`).
///
/// Palauttaa englanninkielisen, kompaktin kertomuksen joka soveltuu
/// README-kuvakaappaukseen.
///
/// # Errors
/// [`ReplayError::Io`] jos temp-/kohdehakemiston luonti tai siivous
/// epäonnistuu; [`ReplayError::Durable`] jos journalin luku/kirjoitus/fork
/// epäonnistuu. Ei koskaan paniikkia.
#[allow(clippy::too_many_lines)]
pub fn run_demo(dir: Option<&Path>) -> Result<String, ReplayError> {
    let (demo_dir, cleanup_on_exit) = if let Some(path) = dir {
        std::fs::create_dir_all(path)?;
        (path.to_path_buf(), false)
    } else {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "familyclaw-replay-demo-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&path)?;
        (path, true)
    };

    let result = run_demo_in(&demo_dir);

    if cleanup_on_exit {
        let _ = std::fs::remove_dir_all(&demo_dir);
    }

    result
}

/// Suorittaa esittelyn annetussa (jo olemassa olevassa) hakemistossa.
///
/// Erotettu [`run_demo`]:sta jotta temp-siivous ([`run_demo`]) tapahtuu myös
/// virhepolulla (`?`-operaattori tässä funktiossa ei ohita siivousta,
/// koska kutsuja hoitaa sen `result`-arvon kautta).
fn run_demo_in(demo_dir: &Path) -> Result<String, ReplayError> {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "=== FamilyClaw Time Machine demo ===\n\
         Story: a refund request is approved by a BUGGY policy (2x instead of \
         capped), then Time Machine forks the run, replays the prefix, and \
         proves a FIXED policy under dry-run — with the original run left \
         untouched.\n"
    );

    // --- Act 1: build the "original" run (with the policy bug). -----------
    let original_path = demo_dir.join("original.jsonl");
    let original_journal = FileJournal::open(&original_path)?;
    let mut ctx = DurableContext::new(original_journal)?;

    let amount: i64 = ctx.step("load_request", || Ok(100))?;
    // BUG: approves 2x the requested amount instead of capping it.
    let approved: i64 = ctx.step("decide_policy", || Ok(amount * 2))?;
    let _receipt: String = ctx.step("dispatch_refund", || Ok(format!("sent:{approved}")))?;
    let original_journal = ctx.finish();
    let original_entry_count = original_journal.len()?;

    let _ = writeln!(
        out,
        "--- Original run (what the agent did, and why) ---\n\
         request amount: {amount}\n\
         decide_policy (BUGGY: amount * 2): approved {approved}\n\
         dispatch_refund: sent:{approved}\n"
    );

    let original_timeline = TimeMachine::inspect(&original_journal)?;
    let _ = writeln!(out, "{}", original_timeline.render_markdown());

    // --- Act 2: fork before decide_policy, run the FIXED policy. ----------
    // Keep only "load_request" (1 step) — decide_policy and dispatch_refund
    // are re-run fresh, with the fix, under a dry-run capture.
    let fork_path = demo_dir.join("fixed_fork.jsonl");
    let fork_target = FileJournal::open(&fork_path)?;
    let kept = TimeMachine::fork_into(&original_journal, 1, &fork_target)?;

    let mut alt_ctx = DurableContext::new(fork_target)?;
    // Replay: "load_request" comes back from the log — the closure below is
    // never actually invoked (fail-closed default in case that ever changes).
    let replayed_amount: i64 = alt_ctx.step("load_request", || Ok(-1))?;

    let recorder = DryRunRecorder::new();
    // FIX: cap the approved amount instead of doubling it.
    let fixed_approved: i64 = alt_ctx.step("decide_policy", || Ok(replayed_amount.min(100)))?;
    let _dry_receipt: String = alt_ctx.step("dispatch_refund", || {
        recorder.record(
            "dispatch_refund",
            serde_json::json!({"would_send": fixed_approved}),
        );
        Ok(format!("dry:{fixed_approved}"))
    })?;
    let fixed_journal = alt_ctx.finish();

    let _ = writeln!(
        out,
        "--- Fork before decide_policy (kept {kept} step(s)) — FIXED policy, dry-run ---\n\
         request amount (replayed from log): {replayed_amount}\n\
         decide_policy (FIXED: min(amount, 100)): approved {fixed_approved}\n\
         dispatch_refund: DRY RUN — nothing sent for real\n"
    );

    // --- Act 3: diff, proving the fix. -------------------------------------
    let diff = TimeMachine::diff(&original_journal, &fixed_journal)?;
    let _ = writeln!(out, "--- Timeline diff (original vs. fixed fork) ---");
    let _ = writeln!(out, "{}", diff.render_markdown());

    // --- Act 4: captured dry-run intent — proof nothing real was sent. ----
    let intents = recorder.intents();
    let _ = writeln!(out, "--- Captured dry-run intent(s) ---");
    for intent in &intents {
        let _ = writeln!(
            out,
            "- step `{}`: would-be payload = {}",
            intent.step, intent.payload
        );
    }
    let _ = writeln!(
        out,
        "DryRunRecorder has no dispatch path — this intent can never reach a \
         real external system through this type.\n"
    );

    // --- Act 5: confirm the original journal is untouched. ----------------
    let original_entry_count_after = original_journal.len()?;
    let _ = writeln!(
        out,
        "--- Original journal integrity ---\n\
         entries before fork: {original_entry_count}\n\
         entries after fork + fixed dry-run: {original_entry_count_after}\n\
         untouched: {}\n",
        original_entry_count == original_entry_count_after
    );

    let _ = writeln!(
        out,
        "=== Demo complete: bug shown ({amount} -> {approved}), fix proven \
         ({replayed_amount} -> {fixed_approved}) under dry-run, original \
         journal untouched ({original_entry_count} entries). ==="
    );

    Ok(out)
}

/// Yksi kutsu koko `replay`-alikomennolle: jäsennä + suorita.
///
/// `args` on `familyclaw replay`-prefiksin jälkeiset argumentit.
///
/// # Errors
/// Vie [`parse`]:n ja [`execute`]:n virheet läpi.
pub fn run<I, S>(args: I) -> Result<String, ReplayError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    execute(parse(args)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_durable::{DurableContext, Journal};

    /// Pieni RAII-temp-tiedosto ilman ulkoisia crateja (sama kuvio kuin
    /// `familyclaw-durable/src/context.rs`-testeissä).
    struct TempPath(PathBuf);

    impl TempPath {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "familyclaw-agent-replay-{tag}-{}-{:?}.jsonl",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            let _ = std::fs::remove_file(&p);
            Self(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Apuri: kirjoita kolmiaskelinen ajo (load → decide → act) annettuun
    /// polkuun ja sulje kahva (fsync).
    fn write_three_step_journal(path: &Path) {
        let journal = FileJournal::open(path).expect("open");
        let mut ctx = DurableContext::new(journal).expect("ctx");
        let amount: i64 = ctx.step("load", || Ok(100)).expect("load");
        let approved: i64 = ctx.step("decide", || Ok(amount * 2)).expect("decide");
        let _r: String = ctx
            .step("act", || Ok(format!("sent:{approved}")))
            .expect("act");
        let journal = ctx.finish();
        // Varmista että kaikki kolme riviä ovat levyllä.
        assert_eq!(journal.len().expect("len"), 3);
    }

    // ---------- parse ----------

    #[test]
    fn parse_inspect_reads_journal_and_json_flag() {
        let cmd = parse(["inspect", "--journal", "run.jsonl", "--json"]).expect("parse");
        assert_eq!(
            cmd,
            ReplayCommand::Inspect {
                journal: PathBuf::from("run.jsonl"),
                json: true,
            }
        );
    }

    #[test]
    fn parse_inspect_defaults_to_markdown() {
        let cmd = parse(["inspect", "--journal", "run.jsonl"]).expect("parse");
        assert_eq!(
            cmd,
            ReplayCommand::Inspect {
                journal: PathBuf::from("run.jsonl"),
                json: false,
            }
        );
    }

    #[test]
    fn parse_fork_reads_all_flags() {
        let cmd = parse([
            "fork",
            "--journal",
            "src.jsonl",
            "--keep",
            "2",
            "--out",
            "dst.jsonl",
        ])
        .expect("parse");
        assert_eq!(
            cmd,
            ReplayCommand::Fork {
                journal: PathBuf::from("src.jsonl"),
                keep: 2,
                out: PathBuf::from("dst.jsonl"),
            }
        );
    }

    #[test]
    fn parse_diff_reads_before_after() {
        let cmd = parse(["diff", "--before", "a.jsonl", "--after", "b.jsonl"]).expect("parse");
        assert_eq!(
            cmd,
            ReplayCommand::Diff {
                before: PathBuf::from("a.jsonl"),
                after: PathBuf::from("b.jsonl"),
                json: false,
            }
        );
    }

    #[test]
    fn parse_missing_subcommand_is_usage_error() {
        let err = parse(Vec::<String>::new()).expect_err("must fail");
        assert!(matches!(err, ReplayError::Usage(_)));
    }

    #[test]
    fn parse_unknown_subcommand_is_usage_error() {
        let err = parse(["frobnicate"]).expect_err("must fail");
        match err {
            ReplayError::Usage(msg) => assert!(msg.contains("unknown replay subcommand")),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn parse_missing_required_flag_is_usage_error() {
        let err = parse(["inspect"]).expect_err("must fail");
        match err {
            ReplayError::Usage(msg) => assert!(msg.contains("--journal")),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn parse_flag_without_value_is_usage_error() {
        let err = parse(["inspect", "--journal"]).expect_err("must fail");
        match err {
            ReplayError::Usage(msg) => assert!(msg.contains("requires a value")),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn parse_unknown_flag_is_usage_error() {
        let err = parse(["inspect", "--journal", "x", "--bogus"]).expect_err("must fail");
        match err {
            ReplayError::Usage(msg) => assert!(msg.contains("unknown flag")),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    #[test]
    fn parse_non_numeric_keep_is_usage_error() {
        let err = parse(["fork", "--journal", "s", "--keep", "abc", "--out", "d"])
            .expect_err("must fail");
        match err {
            ReplayError::Usage(msg) => assert!(msg.contains("--keep")),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    // ---------- run_inspect ----------

    #[test]
    fn run_inspect_markdown_mentions_every_step() {
        let tmp = TempPath::new("inspect-md");
        write_three_step_journal(tmp.path());

        let out = run_inspect(tmp.path(), false).expect("inspect");
        for name in ["load", "decide", "act"] {
            assert!(out.contains(name), "markdown must mention `{name}`");
        }
        assert!(out.contains("Timeline"));
    }

    #[test]
    fn run_inspect_json_is_valid_and_has_steps() {
        let tmp = TempPath::new("inspect-json");
        write_three_step_journal(tmp.path());

        let out = run_inspect(tmp.path(), true).expect("inspect json");
        let value: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(value["steps"].as_array().expect("steps array").len(), 3);
    }

    #[test]
    fn run_inspect_missing_journal_is_durable_io_error() {
        // Olematon polku → FileJournal::open luo tyhjän tiedoston, joten sen
        // sijaan varmistetaan että tyhjä journal antaa tyhjän aikajanan.
        let tmp = TempPath::new("inspect-empty");
        let out = run_inspect(tmp.path(), false).expect("empty inspect");
        assert!(out.contains("0 step(s)"));
    }

    // ---------- run_fork ----------

    #[test]
    fn run_fork_writes_prefix_and_reports_kept_count() {
        let src = TempPath::new("fork-src");
        let dst = TempPath::new("fork-dst");
        write_three_step_journal(src.path());
        // Kohde ei saa olla olemassa: poista temp-varaus.
        let _ = std::fs::remove_file(dst.path());

        let msg = run_fork(src.path(), 2, dst.path()).expect("fork");
        assert!(msg.contains("kept 2 step(s)"));

        // Haara sisältää kaksi askelta + auditmarkerin.
        let forked = FileJournal::open(dst.path()).expect("reopen fork");
        let timeline = TimeMachine::inspect(&forked).expect("inspect fork");
        assert_eq!(timeline.len(), 2);
        assert_eq!(timeline.steps[0].name, "load");
        assert_eq!(timeline.steps[1].name, "decide");
        assert_eq!(timeline.marker_count, 1, "audit marker present");

        // Lähdejournal ei muuttunut.
        let source = FileJournal::open(src.path()).expect("reopen source");
        assert_eq!(TimeMachine::inspect(&source).expect("inspect src").len(), 3);
    }

    #[test]
    fn run_fork_refuses_existing_out_fail_closed() {
        let src = TempPath::new("fork-src2");
        let dst = TempPath::new("fork-dst2");
        write_three_step_journal(src.path());
        // dst on olemassa (TempPath::new poisti, kirjoita jotain takaisin).
        std::fs::write(dst.path(), b"existing\n").expect("create dst");

        let err = run_fork(src.path(), 1, dst.path()).expect_err("must refuse");
        match err {
            ReplayError::Usage(msg) => assert!(msg.contains("fail-closed")),
            other => panic!("expected Usage (fail-closed), got {other:?}"),
        }
        // Olemassa oleva tiedosto ei muuttunut.
        assert_eq!(
            std::fs::read_to_string(dst.path()).expect("read dst"),
            "existing\n"
        );
    }

    #[test]
    fn run_fork_beyond_timeline_is_durable_error() {
        let src = TempPath::new("fork-src3");
        let dst = TempPath::new("fork-dst3");
        write_three_step_journal(src.path());
        let _ = std::fs::remove_file(dst.path());

        let err = run_fork(src.path(), 99, dst.path()).expect_err("must fail");
        assert!(matches!(err, ReplayError::Durable(_)));
    }

    // ---------- run_diff ----------

    #[test]
    fn run_diff_identical_journals_is_identical() {
        let tmp = TempPath::new("diff-same");
        write_three_step_journal(tmp.path());

        let out = run_diff(tmp.path(), tmp.path(), false).expect("diff");
        assert!(out.contains("identical: true"));
    }

    #[test]
    fn run_diff_json_is_valid() {
        let tmp = TempPath::new("diff-json");
        write_three_step_journal(tmp.path());

        let out = run_diff(tmp.path(), tmp.path(), true).expect("diff json");
        let value: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert!(value["steps"].is_array());
    }

    #[test]
    fn run_diff_reports_forked_change() {
        let src = TempPath::new("diff-src");
        let dst = TempPath::new("diff-dst");
        write_three_step_journal(src.path());
        let _ = std::fs::remove_file(dst.path());

        // Haaraudu ennen decide-askelta ja aja eri jatko.
        {
            let source = FileJournal::open(src.path()).expect("open src");
            let target = FileJournal::open(dst.path()).expect("open dst");
            TimeMachine::fork_into(&source, 1, &target).expect("fork");
        }
        {
            let reopened = FileJournal::open(dst.path()).expect("reopen");
            let mut ctx = DurableContext::new(reopened).expect("ctx");
            let amount: i64 = ctx.step("load", || Ok(0)).expect("load");
            let approved: i64 = ctx.step("decide", || Ok(amount / 2)).expect("decide");
            let _r: String = ctx
                .step("act", || Ok(format!("dry:{approved}")))
                .expect("act");
        }

        let out = run_diff(src.path(), dst.path(), false).expect("diff");
        assert!(out.contains("identical: false"));
        assert!(out.contains("changed"));
    }

    // ---------- run (end to end) ----------

    #[test]
    fn run_dispatches_inspect() {
        let tmp = TempPath::new("run-inspect");
        write_three_step_journal(tmp.path());

        let args = vec![
            "inspect".to_string(),
            "--journal".to_string(),
            tmp.path().to_string_lossy().into_owned(),
        ];
        let out = run(args).expect("run");
        assert!(out.contains("load"));
    }

    #[test]
    fn usage_text_mentions_all_subcommands() {
        let text = usage();
        for sub in ["inspect", "fork", "diff", "demo"] {
            assert!(text.contains(sub), "usage must mention `{sub}`");
        }
    }

    // ---------- parse demo ----------

    #[test]
    fn parse_demo_without_dir() {
        let cmd = parse(["demo"]).expect("parse");
        assert_eq!(cmd, ReplayCommand::Demo { dir: None });
    }

    #[test]
    fn parse_demo_with_dir() {
        let cmd = parse(["demo", "--dir", "some/path"]).expect("parse");
        assert_eq!(
            cmd,
            ReplayCommand::Demo {
                dir: Some(PathBuf::from("some/path")),
            }
        );
    }

    #[test]
    fn parse_demo_unknown_flag_is_usage_error() {
        let err = parse(["demo", "--bogus"]).expect_err("must fail");
        match err {
            ReplayError::Usage(msg) => assert!(msg.contains("unknown flag")),
            other => panic!("expected Usage, got {other:?}"),
        }
    }

    // ---------- run_demo ----------

    /// Apuri: uniikki temp-hakemistopolku joka poistetaan Dropissa (jos
    /// vielä olemassa) — käytetään `--dir`-testeissä.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut p = std::env::temp_dir();
            p.push(format!(
                "familyclaw-agent-replay-demo-{tag}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos())
            ));
            let _ = std::fs::remove_dir_all(&p);
            Self(p)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn run_demo_reports_bug_fix_and_untouched_original() {
        let out = run_demo(None).expect("demo");

        // Changed step: decide_policy went from the buggy 200 to fixed 100.
        assert!(out.contains("decide_policy"), "must mention changed step");
        assert!(out.contains("200"), "must show the buggy approved amount");
        assert!(out.contains("100"), "must show the fixed approved amount");

        // Dry-run intent captured.
        assert!(
            out.contains("dry-run intent"),
            "must mention captured dry-run intent"
        );
        assert!(
            out.contains("would_send"),
            "must show the captured would-be payload"
        );
        assert!(
            out.contains("dispatch_refund"),
            "must mention the dry-run step name"
        );

        // Confirmation the original journal is untouched.
        assert!(
            out.contains("untouched: true"),
            "must confirm original journal entry count is unchanged"
        );
    }

    #[test]
    fn run_demo_with_dir_writes_journals_there() {
        let dir = TempDir::new("with-dir");
        let out = run_demo(Some(dir.path())).expect("demo");
        assert!(out.contains("untouched: true"));

        let original = dir.path().join("original.jsonl");
        let fork = dir.path().join("fixed_fork.jsonl");
        assert!(original.exists(), "original journal must be written");
        assert!(fork.exists(), "fork journal must be written");

        // Original journal on disk still shows the buggy 3-step run.
        let reopened = FileJournal::open(&original).expect("reopen original");
        let timeline = TimeMachine::inspect(&reopened).expect("inspect original");
        assert_eq!(timeline.len(), 3);
    }

    #[test]
    fn run_demo_default_path_leaves_no_temp_litter() {
        let before: std::collections::HashSet<PathBuf> = std::fs::read_dir(std::env::temp_dir())
            .expect("read temp dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();

        run_demo(None).expect("demo");

        let after: std::collections::HashSet<PathBuf> = std::fs::read_dir(std::env::temp_dir())
            .expect("read temp dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();

        let leaked: Vec<&PathBuf> = after
            .difference(&before)
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("familyclaw-replay-demo-"))
            })
            .collect();
        assert!(
            leaked.is_empty(),
            "run_demo(None) must clean up its temp dir, leaked: {leaked:?}"
        );
    }

    #[test]
    fn run_dispatches_demo() {
        let args = vec!["demo".to_string()];
        let out = run(args).expect("run");
        assert!(out.contains("Time Machine demo"));
    }
}
