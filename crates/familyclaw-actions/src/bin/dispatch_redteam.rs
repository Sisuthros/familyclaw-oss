//! `dispatch_redteam` — musta laatikko exactly-once-lähetyksen todistamiseen.
//!
//! Tämä binääri ajetaan lapsiprosessina (kuten `continuity_daemon`), jotta
//! "SIGKILL kesken lähetyksen" voidaan todistaa **aidon prosessirajan yli**. Se
//! kohdistuu täsmälleen siihen bugiin jonka GPT-5.5 paljasti: ikkuna jossa
//! [`ActionRuntime`]:n sivuvaikutus (`submit_task`) on jo tapahtunut mutta
//! agenttikerroksen durable-journalointi EI vielä — kaatuminen siinä saa replayn
//! ajamaan sivuvaikutuksen uudelleen (kaksoislaukaisu).
//!
//! ## Sivuvaikutuksen laskuri (todiste)
//! Rekisteröity taito ([`CountingExecutor`]) **kasvattaa levyllä olevaa laskuria
//! joka kerta kun sen `execute` ajetaan oikeasti**. Testi lukee laskurin raakana
//! ja vaatii että se on tasan 1 — kaksoislaukaisu nostaisi sen 2:een.
//!
//! ## Moodit (`--mode`)
//! - `old` — käyttää [`ActionRuntime::submit_task_as`]:ia (bugiton ennen korjausta:
//!   EI outbox-suojaa) → todistaa että bugi ON olemassa (laskuri = 2).
//! - `new` — käyttää [`ActionRuntime::submit_task_idempotent`]:ia kaatumiskestävän
//!   outboxin kanssa → todistaa korjauksen (laskuri = 1, lopputulos identtinen).
//!
//! ## Vaiheet (`--phase`)
//! ### `submit_task`-polku (avaimet `turn-*`)
//! - `crash` — aja lähetys (sivuvaikutus tapahtuu), kirjaa lopputulos
//!   `--outcome-out`-tiedostoon, ja **poistu 137 ENNEN kuin agentti ehtisi
//!   journaloida dispatch-rivin**. Tämä on COMMITTED-ikkuna: outbox on jo
//!   täysin kirjoitettu (intent + committed), vain agenttikerroksen journal-rivi
//!   puuttuu. Hyvänlaatuinen replay-kohta (exactly-once arvo-identtinen).
//! - `crash_intent` — kaadu **INTENT-ONLY-ikkunassa**: `record_intent` on jo
//!   levyllä JA sivuvaikutus on jo lauennut (laskuri = 1), mutta `record_committed`
//!   EI ole vielä ajettu. Tämä on se aidosti vaarallinen ikkuna joka todistaa
//!   **at-most-once fail-closed** -takuun (vrt. moduulin [`CrashAfterIntentOutbox`]).
//! - `resume` — toistaa täsmälleen sen mitä agentin tuore-ajo-haara tekee kun
//!   journal-riviä EI ole (koska kaatuminen esti sen): ajaa SAMAN lähetyksen
//!   samalla idempotenssi-avaimella uudelleen. COMMITTED-ikkunan jälkeen
//!   (`new`-moodi) outbox palauttaa committed-lopputuloksen ajamatta
//!   sivuvaikutusta; `old`-moodissa se ajetaan uudelleen (double-fire).
//! - `resume_intent` — toistaa intent-only-kaatumisen jälkeen: outbox-lookup
//!   palauttaa `InProgress` → `submit_task_idempotent` palauttaa
//!   [`PolicyDenied`](familyclaw_actions::ActionError::PolicyDenied) fail-closed,
//!   eikä sivuvaikutus aja uudelleen (laskuri pysyy 1:ssä).
//!
//! ### Hyväksyntäpolku (avaimet `approval-*`)
//! Tämä todistaa SAMAN at-most-once-takuun [`ActionRuntime::approve`]:n
//! sivuvaikutus-ikkunalle — outbox-avain on `approval-{id}`, EI `turn-*`. Polku
//! tarvitsee kaatumiskestävän **pending**-pinnan (Wire-vaihe), jotta tuore
//! prosessi voi ladata odottavan hyväksynnän levyltä ja **uudelleenhyväksyä
//! saman `ApprovalId`:n**.
//! - `approve_crash_intent` — lähetä hyväksyntää vaativa tehtävä, sitten
//!   `approve()` aseistetulla intent-koukulla: `run_after_approval` ajaa
//!   sivuvaikutuksen (laskuri = 1), `record_intent` on fsyncattu, mutta prosessi
//!   abortoi `record_committed`:n alussa → **poistuu 137 INTENT-ONLY-ikkunassa**
//!   (intent levyllä, committed + `pending.remove` tekemättä).
//! - `approve_crash_committed` — kuten yllä mutta kaatumiskoukku abortoi
//!   `record_committed`:n **jälkeen** (committed fsyncattu) mutta ENNEN
//!   `pending.remove`:a → COMMITTED-ikkuna hyväksyntäpolulla.
//! - `approve_resume` — tuore prosessi lataa odottavan hyväksynnän durable-
//!   pinnalta (Wire), poimii **saman** `ApprovalId`:n ja uudelleenhyväksyy sen:
//!   intent-only-kaatumisen jälkeen outbox näkee `InProgress` →
//!   [`PolicyDenied`](familyclaw_actions::ActionError::PolicyDenied) fail-closed
//!   (laskuri pysyy 1:ssä); committed-kaatumisen jälkeen outbox näkee `Committed`
//!   → arvo-identtinen lopputulos (laskuri pysyy 1:ssä).
//!
//! ## Kaatumiskoukku — tuotannossa SAAVUTTAMATON (turvallisuusperustelu)
//! Intent-only- ja committed-kaatumiset toteutetaan [`CrashAfterIntentOutbox`]-
//! kääreellä joka delegoi oikealle [`JournalDispatchOutbox`]:lle, mutta sen
//! `record_committed` **abortoi prosessin** kun se on aseistettu jommallakummalla
//! ympäristömuuttujalla:
//! - [`CRASH_AFTER_INTENT_ENV`] → abort **ENNEN** delegointia (intent levyllä,
//!   committed kirjoittamatta = INTENT-ONLY-ikkuna).
//! - [`CRASH_AFTER_COMMITTED_ENV`] → abort **JÄLKEEN** delegoinnin (committed
//!   fsyncattu, mutta `pending.remove` ajamatta = COMMITTED-ikkuna
//!   hyväksyntäpolulla).
//!
//! Koska sekä `submit_task_idempotent` että `approve` kutsuvat `record_intent` →
//! sivuvaikutus → `record_committed` tässä järjestyksessä, abort
//! `record_committed`:n ympärillä jättää tilan tasan haluttuun ikkunaan.
//!
//! Koukku on **kaksinkertaisesti portitettu eikä voi laueta tuotannossa**:
//! 1. **Käännösraja:** [`CrashAfterIntentOutbox`] on määritelty VAIN tässä
//!    red-team-binäärissä (`src/bin/`), EI kirjastossa. Tuotantokoodi rakentaa
//!    outboxinsa aina [`JournalDispatchOutbox`]:sta tai
//!    [`InMemoryDispatchOutbox`](familyclaw_actions::dispatch_outbox::InMemoryDispatchOutbox):sta
//!    — tätä kääre-tyyppiä ei ole olemassa kirjasto-API:ssa, joten sitä on
//!    rakenteellisesti mahdotonta instantioida tuotannossa.
//! 2. **Ajonaikainen portti:** vaikka tyyppi jotenkin päätyisi käyttöön, abort
//!    laukeaa vain kun [`CRASH_AFTER_INTENT_ENV`] **tai**
//!    [`CRASH_AFTER_COMMITTED_ENV`] = `"1"`. Mikään tuotantopolku ei aseta
//!    kumpaakaan muuttujaa.
//!
//! ## Determinismi
//! Kello injektoidaan `--clock`:lla — järjestelmäkelloa ei lueta koskaan.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use clap::{Parser, Subcommand, ValueEnum};
use familyclaw_actions::dispatch_outbox::{
    DispatchLookup, DispatchOutboxStore, DispatchedOutcome, JournalDispatchOutbox,
};
use familyclaw_actions::executor::{ActionExecutor, ActionRequest, ActionResult};
use familyclaw_actions::manifest::{default_input_schema, SkillManifest};
use familyclaw_actions::policy::{ActionRisk, ApprovalPolicy, SkillPermission};
use familyclaw_actions::skills::Skill;
use familyclaw_actions::{ActionError, ActionRuntime, SkillId, SubmitOutcome};
use familyclaw_core::{time, Timestamp};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Laskurin kasvatuksen taito.
///
/// Jokainen `execute` kasvattaa **levyllä** olevaa sivuvaikutuslaskuria —
/// tämä on se mittari joka paljastaa kaksoislaukaisun yli prosessirajan.
///
/// Taito on tarkoituksella **auto-run** ([`ActionRisk::ReadOnly`] +
/// [`ApprovalPolicy::AutoIfReadOnly`]), jotta `submit_task` AJAA suorittajan
/// (= sivuvaikutuksen) heti ensimmäisellä kutsulla — eikä jää odottamaan
/// hyväksyntää. Näin "ulkoinen sivuvaikutus" tapahtuu mitattavasti jokaisella
/// `submit_task`-suorituksella, ja kaksoislaukaisu näkyy laskurissa suoraan.
#[derive(Debug)]
struct CountingExecutor {
    /// Polku jossa sivuvaikutuslaskuri elää (luetaan + kirjoitetaan joka ajolla).
    counter_path: PathBuf,
    /// Prosessin sisäinen laskuri (diagnostiikka; varsinainen todiste on levyllä).
    in_process: AtomicU64,
}

impl CountingExecutor {
    /// Kiinteä tunniste, jotta `start` ja `resume` viittaavat samaan taitoon.
    const SKILL_UUID: Uuid = uuid::uuid!("11111111-2222-4333-8444-555566667777");

    fn skill_id() -> SkillId {
        SkillId::from_uuid(Self::SKILL_UUID)
    }

    fn new(counter_path: PathBuf) -> Self {
        Self {
            counter_path,
            in_process: AtomicU64::new(0),
        }
    }

    /// Kasvattaa levyllä olevaa sivuvaikutuslaskuria atomisesti (luku → +1 → kirjoitus).
    fn bump_disk_counter(&self) {
        let current = std::fs::read_to_string(&self.counter_path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let _ = std::fs::write(&self.counter_path, (current + 1).to_string());
    }
}

#[async_trait]
impl ActionExecutor for CountingExecutor {
    async fn execute(&self, request: ActionRequest) -> familyclaw_actions::Result<ActionResult> {
        // SIVUVAIKUTUS: kasvata laskuria. Tämä on "ulkoinen vaikutus" jonka on
        // tapahduttava tasan kerran SIGKILL:n yli.
        self.in_process.fetch_add(1, Ordering::SeqCst);
        self.bump_disk_counter();
        Ok(ActionResult::success(
            "counter bumped",
            serde_json::json!({ "ok": true }),
            request.now,
        ))
    }
}

impl Skill for CountingExecutor {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "counting_side_effect".to_string(),
            version: "1.0.0".to_string(),
            description: "Kasvattaa sivuvaikutuslaskuria (auto-run, suoritetaan heti)."
                .to_string(),
            permissions: vec![SkillPermission::NetworkRead],
            risk: ActionRisk::ReadOnly,
            approval_policy: ApprovalPolicy::AutoIfReadOnly,
            input_hint: None,
            output_hint: None,
            input_schema: default_input_schema(),
        }
    }
}

/// Hyväksyntää vaativa laskuri-taito (hyväksyntäpolun sivuvaikutus).
///
/// Identtinen [`CountingExecutor`]:n kanssa PAITSI että sen riskiluokka on
/// [`ActionRisk::WriteExternal`] → `submit_task` jättää tehtävän odottamaan
/// ihmisen hyväksyntää sen sijaan että ajaisi sen heti. Sivuvaikutus (laskurin
/// kasvatus) tapahtuu siis vasta [`ActionRuntime::approve`]:n ajaessa
/// [`run_after_approval`](familyclaw_actions)-haaran — täsmälleen se ikkuna jonka
/// at-most-once-takuu kattaa `approval-{id}`-avaimella.
///
/// Laskuri elää **levyllä** samalla mekanismilla kuin [`CountingExecutor`]:lla,
/// joten kaksoislaukaisu hyväksynnän yli näkyy laskurissa suoraan (1 → 2).
#[derive(Debug)]
struct ApprovalCountingExecutor {
    /// Polku jossa sivuvaikutuslaskuri elää (jaettu muoto [`CountingExecutor`]:n kanssa).
    counter_path: PathBuf,
    /// Prosessin sisäinen laskuri (diagnostiikka; varsinainen todiste on levyllä).
    in_process: AtomicU64,
}

impl ApprovalCountingExecutor {
    /// Kiinteä tunniste (eri kuin [`CountingExecutor`]:lla), jotta hyväksyntäpolun
    /// taito on yksikäsitteinen rekisterissä.
    const SKILL_UUID: Uuid = uuid::uuid!("99999999-8888-4777-8666-555544443333");

    fn skill_id() -> SkillId {
        SkillId::from_uuid(Self::SKILL_UUID)
    }

    fn new(counter_path: PathBuf) -> Self {
        Self {
            counter_path,
            in_process: AtomicU64::new(0),
        }
    }

    /// Kasvattaa levyllä olevaa sivuvaikutuslaskuria atomisesti (luku → +1 → kirjoitus).
    fn bump_disk_counter(&self) {
        let current = std::fs::read_to_string(&self.counter_path)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let _ = std::fs::write(&self.counter_path, (current + 1).to_string());
    }
}

#[async_trait]
impl ActionExecutor for ApprovalCountingExecutor {
    async fn execute(&self, request: ActionRequest) -> familyclaw_actions::Result<ActionResult> {
        // SIVUVAIKUTUS: kasvata laskuria. Ajetaan hyväksyntäpolulla VASTA
        // `approve()`:n `run_after_approval`-haarassa — tämä on se "ulkoinen
        // vaikutus" jonka on tapahduttava korkeintaan kerran SIGKILL:n yli.
        self.in_process.fetch_add(1, Ordering::SeqCst);
        self.bump_disk_counter();
        Ok(ActionResult::success(
            "counter bumped (approval path)",
            serde_json::json!({ "ok": true }),
            request.now,
        ))
    }
}

impl Skill for ApprovalCountingExecutor {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "counting_side_effect_approval".to_string(),
            version: "1.0.0".to_string(),
            description: "Kasvattaa sivuvaikutuslaskuria (vaatii hyväksynnän)."
                .to_string(),
            permissions: vec![SkillPermission::WriteExternal],
            // WriteExternal → vaatii ihmisen hyväksynnän (ei auto-run).
            risk: ActionRisk::WriteExternal,
            approval_policy: ApprovalPolicy::RequireApproval,
            input_hint: None,
            output_hint: None,
            input_schema: default_input_schema(),
        }
    }
}

/// Ympäristömuuttuja joka **aseistaa** intent-only-kaatumiskoukun.
///
/// Vain kun tämä on `"1"`, [`CrashAfterIntentOutbox::record_committed`] abortoi
/// prosessin ennen delegointia. Mikään tuotantopolku ei aseta tätä — ks. moduulin
/// dokumentaatio (käännösraja + ajonaikainen portti).
const CRASH_AFTER_INTENT_ENV: &str = "FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT";

/// Ympäristömuuttuja joka **aseistaa** committed-ikkunan kaatumiskoukun.
///
/// Vain kun tämä on `"1"`, [`CrashAfterIntentOutbox::record_committed`] abortoi
/// prosessin **delegoinnin JÄLKEEN** (committed on jo fsyncattu levylle) mutta
/// ennen kuin kutsuja ehtii `pending.remove`:n. Tämä jäljittelee
/// hyväksyntäpolun COMMITTED-ikkunaa. Mikään tuotantopolku ei aseta tätä — ks.
/// moduulin dokumentaatio (käännösraja + ajonaikainen portti).
const CRASH_AFTER_COMMITTED_ENV: &str = "FAMILYCLAW_REDTEAM_CRASH_AFTER_COMMITTED";

/// Exit-koodi jolla intent-only-kaatuminen poistuu (SIGKILL-tyyli, kuten 137).
const CRASH_EXIT_CODE: i32 = 137;

/// Kaatumiskoukku-kääre joka pakottaa joko **intent-only-** tai
/// **committed-ikkunan** prosessirajan yli.
///
/// Delegoi kaiken oikealle [`JournalDispatchOutbox`]:lle PAITSI että
/// [`record_committed`](CrashAfterIntentOutbox::record_committed) **abortoi
/// prosessin** kun koukku on aseistettu:
/// - [`CRASH_AFTER_INTENT_ENV`] = `"1"` → abort **ENNEN** delegointia: committed
///   EI koskaan kirjoitu (intent levyllä, sivuvaikutus lauennut, committed
///   kirjoittamatta) — INTENT-ONLY-ikkuna jonka GPT-5.5 nosti esiin.
/// - [`CRASH_AFTER_COMMITTED_ENV`] = `"1"` → abort **JÄLKEEN** delegoinnin:
///   committed on jo fsyncattu levylle mutta kutsuja ei ehdi `pending.remove`:a
///   → COMMITTED-ikkuna (hyvänlaatuinen replay-kohta, arvo-identtinen).
///
/// Koska sekä [`ActionRuntime::submit_task_idempotent`] että
/// [`ActionRuntime::approve`] kutsuvat `record_intent` → sivuvaikutus →
/// `record_committed` tässä järjestyksessä, abort `record_committed`:n ympärillä
/// jättää tilan tasan haluttuun ikkunaan.
///
/// ## Tuotannossa saavuttamaton
/// Tämä tyyppi elää VAIN red-team-binäärissä (`src/bin/`), ei kirjasto-API:ssa.
/// Tuotanto rakentaa outboxinsa aina suoraan [`JournalDispatchOutbox`]:sta, joten
/// tätä kääre-tyyppiä ei voi instantioida tuotannossa. Lisäksi abort on portitettu
/// ajonaikaisella ympäristömuuttujalla. Kaksinkertainen suoja → ei voi laueta
/// tuotannossa.
#[derive(Debug)]
struct CrashAfterIntentOutbox {
    /// Oikea kaatumiskestävä outbox johon kaikki ei-abortoivat kutsut delegoidaan.
    inner: JournalDispatchOutbox,
    /// Aseistettu tila intent-only-ikkunaan (abort ENNEN delegointia).
    armed_before: bool,
    /// Aseistettu tila committed-ikkunaan (abort JÄLKEEN delegoinnin).
    armed_after: bool,
}

impl CrashAfterIntentOutbox {
    /// Käärii oikean outboxin ja lukee aseistukset ympäristöstä KERRAN.
    fn new(inner: JournalDispatchOutbox) -> Self {
        let armed_before = std::env::var(CRASH_AFTER_INTENT_ENV).as_deref() == Ok("1");
        let armed_after = std::env::var(CRASH_AFTER_COMMITTED_ENV).as_deref() == Ok("1");
        Self {
            inner,
            armed_before,
            armed_after,
        }
    }
}

impl DispatchOutboxStore for CrashAfterIntentOutbox {
    fn kind(&self) -> &'static str {
        // Kääre delegoi kaiken kaatumiskestävään outboxiin → sama lajitunniste.
        self.inner.kind()
    }

    fn lookup(&self, key: &str) -> familyclaw_actions::Result<DispatchLookup> {
        self.inner.lookup(key)
    }

    fn record_intent(&self, key: &str) -> familyclaw_actions::Result<()> {
        // Aie delegoituu normaalisti (fsync) — tämä on se rivi joka jää levylle
        // intent-only-kaatumisen jälkeen.
        self.inner.record_intent(key)
    }

    fn record_committed(
        &self,
        key: &str,
        outcome: &DispatchedOutcome,
    ) -> familyclaw_actions::Result<()> {
        if self.armed_before {
            // INTENT-ONLY-IKKUNA: record_intent on jo levyllä JA sivuvaikutus on jo
            // lauennut (kutsuja ajoi sen ennen tätä). Abortoidaan ENNEN delegointia
            // → committed EI koskaan kirjoitu. Tämä on aidosti vaarallinen ikkuna.
            //
            // `std::process::exit(137)` jäljittelee SIGKILL:iä — eikä kirjasto
            // koskaan näe committed-riviä.
            let _ = std::io::stderr().flush();
            eprintln!(
                "crash injected: AFTER record_intent + side effect, \
                 BEFORE record_committed (intent-only window)"
            );
            // Käytä eksplisiittistä exit-koodia jotta testi voi vaatia 137:n.
            std::process::exit(CRASH_EXIT_CODE);
        }
        // Committed delegoidaan oikealle outboxille (fsync). Tämän jälkeen
        // committed-marker on levyllä — at-most-once-takuu pitää siitä eteenpäin.
        self.inner.record_committed(key, outcome)?;
        if self.armed_after {
            // COMMITTED-IKKUNA: committed on jo fsyncattu, mutta kutsuja (esim.
            // `approve`) ei ole vielä ehtinyt `pending.remove`:a. Abortoidaan tähän
            // → uudelleenhyväksyntä näkee Committed-rivin ja palauttaa
            // arvo-identtisen lopputuloksen ajamatta sivuvaikutusta uudelleen.
            let _ = std::io::stderr().flush();
            eprintln!(
                "crash injected: AFTER record_committed (committed on disk), \
                 BEFORE pending.remove (committed window)"
            );
            std::process::exit(CRASH_EXIT_CODE);
        }
        Ok(())
    }
}

/// Moodi: vanha (bugi) vai uusi (korjattu) lähetyspolku.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
enum Mode {
    /// `submit_task_as` — EI outbox-idempotenssia (bugi ennen korjausta).
    Old,
    /// `submit_task_idempotent` — outbox-suojattu (korjaus).
    New,
}

/// Vaihe: kaadu kesken vai jatka.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
enum Phase {
    /// COMMITTED-ikkuna: aja lähetys (intent + sivuvaikutus + committed), kirjaa
    /// lopputulos, poistu 137 ENNEN agenttikerroksen journalointia.
    Crash,
    /// INTENT-ONLY-ikkuna: aja lähetys mutta abortoi `record_committed`:n alussa
    /// → intent levyllä + sivuvaikutus lauennut, committed kirjoittamatta.
    /// Vaatii `--mode new` (outbox-suojattu polku) + aseistetun koukun.
    CrashIntent,
    /// COMMITTED-ikkunan jälkeen: aja SAMA lähetys uudelleen (agentin tuore-haara
    /// ilman journal-riviä). Outbox palauttaa committed-lopputuloksen.
    Resume,
    /// INTENT-ONLY-ikkunan jälkeen: aja SAMA lähetys uudelleen → outbox-lookup
    /// palauttaa `InProgress`, joten odotettu lopputulos on `PolicyDenied`
    /// fail-closed (sivuvaikutus EI aja uudelleen).
    ResumeIntent,
    /// HYVÄKSYNTÄPOLKU, INTENT-ONLY-ikkuna: lähetä hyväksyntää vaativa tehtävä,
    /// kirjaa `ApprovalId` levylle, sitten `approve()` aseistetulla intent-koukulla
    /// → `run_after_approval` ajaa sivuvaikutuksen (laskuri = 1), `record_intent`
    /// fsyncattu, prosessi abortoi `record_committed`:n alussa → poistuu 137.
    /// Vaatii `--mode new`, durable pending (`--pending`) + task queue
    /// (`--task-queue`) ja `FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT=1`.
    ApproveCrashIntent,
    /// HYVÄKSYNTÄPOLKU, COMMITTED-ikkuna: kuten yllä mutta koukku abortoi
    /// `record_committed`:n **jälkeen** (committed levyllä) ennen `pending.remove`:a
    /// → poistuu 137. Vaatii `FAMILYCLAW_REDTEAM_CRASH_AFTER_COMMITTED=1`.
    ApproveCrashCommitted,
    /// HYVÄKSYNTÄPOLUN jälkeen: tuore prosessi lataa odottavan hyväksynnän
    /// durable-pinnalta (Wire), poimii SAMAN `ApprovalId`:n ja uudelleenhyväksyy
    /// sen. Intent-only-kaatumisen jälkeen → `PolicyDenied` fail-closed (laskuri
    /// pysyy 1:ssä); committed-kaatumisen jälkeen → arvo-identtinen `SubmitOutcome`
    /// (laskuri pysyy 1:ssä). Koukkua EI aseisteta tässä vaiheessa.
    ApproveResume,
}

/// Komentorivirajapinta.
#[derive(Parser)]
#[command(name = "dispatch_redteam", about = "FamilyClaw exactly-once dispatch black box")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Ainoa alikomento: `run` (vaiheet erotellaan `--phase`:lla).
#[derive(Subcommand)]
enum Command {
    /// Aja yksi vaihe annetussa moodissa.
    Run(RunArgs),
}

/// `run`-argumentit.
#[derive(Parser)]
struct RunArgs {
    /// Vanha (bugi) vai uusi (korjattu) polku.
    #[arg(long, value_enum)]
    mode: Mode,
    /// Vaihe (`crash` / `resume`).
    #[arg(long, value_enum)]
    phase: Phase,
    /// Outbox-journalin polku (kaatumiskestävä idempotenssi).
    #[arg(long)]
    outbox: PathBuf,
    /// Sivuvaikutuslaskurin polku (todiste).
    #[arg(long)]
    counter: PathBuf,
    /// Tiedosto johon `crash`-vaihe kirjaa lopputuloksen (arvo-identtisyyden todiste).
    #[arg(long)]
    outcome_out: PathBuf,
    /// Stabiili idempotenssi-avain (= agentin `turn-{turn}-dispatch-{k}`).
    #[arg(long, default_value = "turn-0-dispatch-0")]
    key: String,
    /// Injektoitu seinäkello (RFC 3339).
    #[arg(long)]
    clock: String,
    /// Kaatumiskestävän **odottavien hyväksyntöjen** pinnan polku (Wire-vaihe).
    /// Pakollinen hyväksyntäpolun vaiheille (`approve_*`).
    #[arg(long)]
    pending: Option<PathBuf>,
    /// Kaatumiskestävän **tehtäväjonon** polku (durable queue). Pakollinen
    /// hyväksyntäpolun vaiheille (`approve_*`).
    #[arg(long)]
    task_queue: Option<PathBuf>,
}

/// Lopputuloksen levymuoto arvo-identtisyyden vertailuun.
#[derive(Debug, Serialize, Deserialize)]
struct OutcomeRecord {
    task_id: String,
    pending_approval: Option<String>,
    status: String,
}

impl OutcomeRecord {
    fn from_submit(outcome: &SubmitOutcome) -> Self {
        Self {
            task_id: outcome.task_id.to_string(),
            pending_approval: outcome.pending_approval.map(|a| a.to_string()),
            status: format!("{:?}", outcome.status),
        }
    }
}

/// Daemonin virhetyyppi.
#[derive(Debug, thiserror::Error)]
enum HarnessError {
    #[error("core error: {0}")]
    Core(#[from] familyclaw_core::FamilyClawError),
    #[error("action error: {0}")]
    Action(#[from] familyclaw_actions::ActionError),
    #[error("serde error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

type HarnessResult<T> = std::result::Result<T, HarnessError>;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(std::io::stderr(), "dispatch_redteam error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> HarnessResult<()> {
    match cli.command {
        Command::Run(args) => run_phase(args).await,
    }
}

/// Rakentaa ajoympäristön: laskuri-taito rekisteröitynä + (uusi-moodissa)
/// kaatumiskestävä outbox annetusta polusta.
///
/// `crash_intent`-vaiheessa kaatumiskestävä outbox kääritään
/// [`CrashAfterIntentOutbox`]:iin, joka abortoi prosessin `record_committed`:n
/// alussa (intent-only-ikkuna). Kaikissa muissa vaiheissa käytetään suoraa
/// [`JournalDispatchOutbox`]:a.
fn build_runtime(args: &RunArgs) -> HarnessResult<ActionRuntime> {
    let mut runtime = ActionRuntime::new();
    runtime.register_skill(CountingExecutor::new(args.counter.clone()))?;
    if args.mode == Mode::New {
        let outbox = JournalDispatchOutbox::open(&args.outbox)?;
        if args.phase == Phase::CrashIntent {
            // Kääri kaatumiskoukku: record_committed abortoi ennen delegointia
            // (kun aseistettu ympäristömuuttujalla).
            runtime = runtime.with_dispatch_outbox(Box::new(CrashAfterIntentOutbox::new(outbox)));
        } else {
            runtime = runtime.with_dispatch_outbox(Box::new(outbox));
        }
    }
    Ok(runtime)
}

/// Pakottaa pakollisen polun argumentin (hyväksyntäpolun vaiheille).
fn require_path<'a>(value: Option<&'a PathBuf>, flag: &str) -> HarnessResult<&'a PathBuf> {
    value.ok_or_else(|| {
        HarnessError::Io(std::io::Error::other(format!(
            "hyväksyntäpolun vaihe vaatii argumentin {flag}"
        )))
    })
}

/// Rakentaa **kaatumiskestävän** ajoympäristön hyväksyntäpolulle: durable pending
/// (Wire) + durable task queue + kaatumiskestävä dispatch-outbox.
///
/// Tämä on se kokoonpano jonka ansiosta tuore prosessi voi ladata odottavan
/// hyväksynnän levyltä ([`ActionRuntime::with_durable_stores`]) ja
/// uudelleenhyväksyä SAMAN `ApprovalId`:n at-most-once-suojan alla
/// (`approval-{id}`-avain). Crash-vaiheissa outbox kääritään
/// [`CrashAfterIntentOutbox`]:iin (intent- tai committed-ikkuna ympäristömuuttujan
/// mukaan); resume-vaiheessa käytetään suoraa [`JournalDispatchOutbox`]:a.
async fn build_approval_runtime(args: &RunArgs) -> HarnessResult<ActionRuntime> {
    let pending = require_path(args.pending.as_ref(), "--pending")?;
    let task_queue = require_path(args.task_queue.as_ref(), "--task-queue")?;

    let outbox = JournalDispatchOutbox::open(&args.outbox)?;
    let wrapped: Box<dyn DispatchOutboxStore> = if matches!(
        args.phase,
        Phase::ApproveCrashIntent | Phase::ApproveCrashCommitted
    ) {
        // Kääri kaatumiskoukku: abortoi record_committed:n ympärillä
        // (ympäristömuuttuja valitsee ennen/jälkeen).
        Box::new(CrashAfterIntentOutbox::new(outbox))
    } else {
        Box::new(outbox)
    };

    let mut runtime = ActionRuntime::with_durable_stores(pending, task_queue)
        .await?
        .with_dispatch_outbox(wrapped);
    runtime.register_skill(ApprovalCountingExecutor::new(args.counter.clone()))?;
    Ok(runtime)
}

/// Ajaa lähetyksen valitulla polulla (vanha vs uusi).
async fn dispatch(
    runtime: &mut ActionRuntime,
    args: &RunArgs,
    now: Timestamp,
) -> familyclaw_actions::Result<SubmitOutcome> {
    let payload = serde_json::json!({ "n": 1 });
    match args.mode {
        // VANHA polku: suora `submit_task_as` ilman idempotenssi-avainta. Tämä on
        // koodi joka oli ennen korjausta — sillä EI ole outbox-suojaa, joten
        // re-drive kaatumisen jälkeen ajaa sivuvaikutuksen uudelleen.
        Mode::Old => {
            runtime
                .submit_task_as("agent_a", CountingExecutor::skill_id(), payload, now)
                .await
        }
        // UUSI polku: idempotentti lähetys vakaalla avaimella. Outbox palauttaa
        // committed-lopputuloksen ajamatta sivuvaikutusta uudelleen.
        Mode::New => {
            runtime
                .submit_task_idempotent(
                    &args.key,
                    "agent_a",
                    CountingExecutor::skill_id(),
                    payload,
                    now,
                )
                .await
        }
    }
}

async fn run_phase(args: RunArgs) -> HarnessResult<()> {
    let now = time::parse_rfc3339(&args.clock)?;

    // Hyväksyntäpolun vaiheet käyttävät eri (kaatumiskestävää) kokoonpanoa ja
    // erillistä taitoa → eroteta ne ENNEN submit-polun ajoympäristön rakennusta.
    if matches!(
        args.phase,
        Phase::ApproveCrashIntent | Phase::ApproveCrashCommitted | Phase::ApproveResume
    ) {
        return run_approval_phase(args, now).await;
    }

    let mut runtime = build_runtime(&args)?;

    match args.phase {
        Phase::ApproveCrashIntent | Phase::ApproveCrashCommitted | Phase::ApproveResume => {
            // Nämä haarautuivat jo `run_approval_phase`:een yllä; tänne ei pitäisi
            // koskaan päästä. Epäonnistu äänekkäästi panikoimatta.
            Err(HarnessError::Io(std::io::Error::other(
                "approval phase reached submit-path match — internal routing error",
            )))
        }
        Phase::Crash => {
            // COMMITTED-ikkuna. Lähetys ajetaan kokonaan (intent + sivuvaikutus +
            // committed). Kirjaa lopputulos arvo-identtisyyden vertailuun ja poistu
            // 137 ENNEN kuin agentti ehtisi journaloida dispatch-rivin.
            let outcome = dispatch(&mut runtime, &args, now).await?;
            write_outcome(&args.outcome_out, &outcome)?;
            eprintln!("crash injected: after committed, before dispatch journal append");
            std::process::exit(CRASH_EXIT_CODE);
        }
        Phase::CrashIntent => {
            // INTENT-ONLY-ikkuna. `dispatch` ei koskaan palaa: kaatumiskoukku
            // abortoi prosessin `record_committed`:n alussa — record_intent on jo
            // levyllä ja sivuvaikutus on jo lauennut. Jos koukku EI ole aseistettu
            // (ympäristömuuttuja puuttuu), tämä on ohjelmointivirhe — älä jätä
            // hiljaa "onnistumaan" vaan epäonnistu äänekkäästi.
            let _ = dispatch(&mut runtime, &args, now).await?;
            Err(HarnessError::Io(std::io::Error::other(
                "crash_intent phase returned without aborting — \
                 is FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT=1 set?",
            )))
        }
        Phase::Resume => {
            // COMMITTED-ikkunan jälkeen: agentin tuore-haaran uudelleenajo
            // (journal-riviä ei ole, koska kaatuminen esti sen). `new`-moodissa
            // outbox neutraloi sen; `old`-moodissa sivuvaikutus tapahtuu toistamiseen.
            let outcome = dispatch(&mut runtime, &args, now).await?;
            let before = read_outcome(&args.outcome_out);
            let now_record = OutcomeRecord::from_submit(&outcome);
            let value_identical = before
                .as_ref()
                .is_some_and(|b| b.task_id == now_record.task_id);
            let result = serde_json::json!({
                "side_effect_count": read_counter(&args.counter),
                "value_identical": value_identical,
                "resumed_task_id": now_record.task_id,
                "crashed_task_id": before.map(|b| b.task_id),
            });
            println!("RESULT {result}");
            std::io::stdout().flush()?;
            Ok(())
        }
        Phase::ResumeIntent => {
            // INTENT-ONLY-ikkunan jälkeen: aja SAMA lähetys samalla avaimella.
            // Outbox-lookup näkee intentin ilman committedia → InProgress →
            // submit_task_idempotent palauttaa PolicyDenied fail-closed. ÄLÄ aja
            // sivuvaikutusta uudelleen. Tämä on at-most-once-takuun ydin.
            let dispatch_result = dispatch(&mut runtime, &args, now).await;
            let policy_denied = matches!(dispatch_result, Err(ActionError::PolicyDenied(_)));
            let denied_message = match &dispatch_result {
                Err(ActionError::PolicyDenied(msg)) => Some(msg.clone()),
                _ => None,
            };
            // Tulosta yhden rivin RESULT-JSON harnessille (laskuri MUST pysyä 1:ssä).
            let result = serde_json::json!({
                "side_effect_count": read_counter(&args.counter),
                "policy_denied": policy_denied,
                "denied_message": denied_message,
                "other_outcome": dispatch_result.ok().map(|o| OutcomeRecord::from_submit(&o).task_id),
            });
            println!("RESULT {result}");
            std::io::stdout().flush()?;
            Ok(())
        }
    }
}

/// Ajaa **hyväksyntäpolun** vaiheet aidon prosessirajan yli.
///
/// Kaikki kolme vaihetta jakavat saman kaatumiskestävän kokoonpanon
/// ([`build_approval_runtime`]): durable pending (Wire) + durable task queue +
/// kaatumiskestävä dispatch-outbox. Idempotenssi-avain on `approval-{id}` (EI
/// `turn-*`).
///
/// - `approve_crash_intent` / `approve_crash_committed`: lähetä hyväksyntää
///   vaativa tehtävä, kirjaa `ApprovalId` + lopputulos levylle, sitten `approve()`
///   aseistetulla koukulla → prosessi abortoi `record_committed`:n ympärillä
///   (intent- tai committed-ikkuna) ja poistuu 137.
/// - `approve_resume`: lataa odottava hyväksyntä durable-pinnalta, poimi SAMA
///   `ApprovalId` ja uudelleenhyväksy se. Tulosta yhden rivin RESULT-JSON.
async fn run_approval_phase(args: RunArgs, now: Timestamp) -> HarnessResult<()> {
    let mut runtime = build_approval_runtime(&args).await?;

    match args.phase {
        Phase::ApproveCrashIntent | Phase::ApproveCrashCommitted => {
            // 1) Lähetä hyväksyntää vaativa tehtävä (WriteExternal → NeedsApproval).
            //    Sivuvaikutus EI vielä laukea — se odottaa hyväksyntää.
            let submitted = runtime
                .submit_task_as(
                    "agent_a",
                    ApprovalCountingExecutor::skill_id(),
                    serde_json::json!({ "n": 1 }),
                    now,
                )
                .await?;
            let approval_id = submitted.pending_approval.ok_or_else(|| {
                HarnessError::Io(std::io::Error::other(
                    "submit ei jättänyt tehtävää odottamaan hyväksyntää \
                     (odotettiin NeedsApproval)",
                ))
            })?;
            // Kirjaa lopputulos + ApprovalId levylle: resume-vaihe vertaa tähän
            // (arvo-identtisyys) ja varmistaa että SAMA hyväksyntä ladattiin.
            write_outcome(&args.outcome_out, &submitted)?;

            // 2) Hyväksy → run_after_approval ajaa sivuvaikutuksen (laskuri = 1),
            //    record_intent fsyncataan, sitten kaatumiskoukku abortoi
            //    record_committed:n ympärillä. `approve` ei palaa normaalisti.
            let _ = runtime.approve(approval_id, now).await?;
            // Jos koukku EI ollut aseistettu, tänne päästään → ohjelmointivirhe.
            Err(HarnessError::Io(std::io::Error::other(
                "approve crash phase returned without aborting — is \
                 FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT / _AFTER_COMMITTED=1 set?",
            )))
        }
        Phase::ApproveResume => {
            // Lataa odottava hyväksyntä durable-pinnalta (Wire-vaihe): tämä on se
            // kohta jossa SAMA ApprovalId rekonstruoidaan levyltä uudessa prosessissa.
            let pending = runtime.try_pending_approvals()?;
            let approval_id = pending.first().map(|p| p.approval_id).ok_or_else(|| {
                HarnessError::Io(std::io::Error::other(
                    "tuore prosessi ei löytänyt odottavaa hyväksyntää durable-pinnalta \
                     (Wire-vaihe rikki?)",
                ))
            })?;

            // Uudelleenhyväksy SAMA ApprovalId → outbox-avain approval-{id}.
            // Intent-only-kaatumisen jälkeen: InProgress → PolicyDenied.
            // Committed-kaatumisen jälkeen: Committed → arvo-identtinen lopputulos.
            let approve_result = runtime.approve(approval_id, now).await;
            let policy_denied = matches!(approve_result, Err(ActionError::PolicyDenied(_)));
            let denied_message = match &approve_result {
                Err(ActionError::PolicyDenied(msg)) => Some(msg.clone()),
                _ => None,
            };

            // Arvo-identtisyys committed-ikkunalle: vertaa kaatuneeseen lopputulokseen.
            let before = read_outcome(&args.outcome_out);
            let resumed = approve_result.as_ref().ok().map(OutcomeRecord::from_submit);
            let value_identical = match (&before, &resumed) {
                (Some(b), Some(r)) => b.task_id == r.task_id,
                _ => false,
            };

            let result = serde_json::json!({
                "side_effect_count": read_counter(&args.counter),
                "policy_denied": policy_denied,
                "denied_message": denied_message,
                "value_identical": value_identical,
                "reloaded_approval_id": approval_id.to_string(),
                "resumed_task_id": resumed.as_ref().map(|r| r.task_id.clone()),
                "resumed_status": resumed.as_ref().map(|r| r.status.clone()),
            });
            println!("RESULT {result}");
            std::io::stdout().flush()?;
            Ok(())
        }
        // Submit-polun vaiheet eivät koskaan päädy tänne (haaroitettu run_phase:ssa).
        Phase::Crash | Phase::CrashIntent | Phase::Resume | Phase::ResumeIntent => {
            Err(HarnessError::Io(std::io::Error::other(
                "submit phase reached approval-path handler — internal routing error",
            )))
        }
    }
}

/// Kirjoittaa lopputuloksen levylle (arvo-identtisyyden todiste).
fn write_outcome(path: &Path, outcome: &SubmitOutcome) -> HarnessResult<()> {
    let record = OutcomeRecord::from_submit(outcome);
    std::fs::write(path, serde_json::to_string(&record)?)?;
    Ok(())
}

/// Lukee aiemmin kirjatun lopputuloksen (jos on).
fn read_outcome(path: &Path) -> Option<OutcomeRecord> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Lukee sivuvaikutuslaskurin raakana (0 jos tiedostoa ei ole).
fn read_counter(path: &Path) -> u64 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}
