//! Realistiset mock-taidot ja niiden yhteinen putki (KERROS A, OSS).
//!
//! Tämä alimoduuli kokoaa neljä realistista **mock-taitoa** ([`Skill`])
//! sekä putken ([`Pipeline`]), joka ajaa koko toimintopinon:
//!
//! ```text
//! observe → plan → request approval (jos tarpeen) → execute action
//!         → verify → persist proof → remember → report
//! ```
//!
//! Taidot ovat tarkoituksella **hermeettisiä**: ne eivät tee oikeita
//! Gmail-/GitHub-verkkokutsuja, vaan tuottavat deterministisen tuloksen
//! syötteestä. Jokainen taito tarjoaa oman [`SkillManifest`]-manifestinsa
//! ([`Skill::manifest`]) ja toteuttaa [`ActionExecutor`]-rajapinnan
//! suorituslogiikalle.
//!
//! ## Putki ([`Pipeline`])
//! [`Pipeline`] sitoo yhteen rekisterin ([`SkillRegistry`]), tehtäväjonon
//! ([`TaskQueue`]), käytäntökerroksen ([`crate::policy`]), hyväksyntärekisterin
//! ([`ApprovalLedger`]), suorittajat ([`ActionExecutor`]), todistepaketit
//! ([`crate::proof`]) sekä audit-keräimen ([`AuditCollector`]). Lopputuloksena
//! syntyy [`PipelineOutcome`], joka kertoo päätyikö tehtävä tilaan
//! [`TaskStatus::Done`] vai jäikö se odottamaan hyväksyntää
//! ([`TaskStatus::NeedsApproval`]).
//!
//! ## Turvaperiaatteet jotka putki valvoo
//! - **Käytäntö johdetaan AINA manifestista** — ei koskaan tehtävän
//!   payloadista. Payloadiin upotettu kehotehyökkäys (prompt injection) ei voi
//!   muuttaa riskiluokkaa eikä ohittaa hyväksyntää.
//! - **Muistiin talletetaan vain redaktoitu yhteenveto** — ei raakaa syötettä
//!   eikä salaisuuksia ([`PipelineOutcome::memory_record`]).
//! - **Taint säilyy** — epäluotettavasta lähteestä (esim. MCP) tullut arvo
//!   pysyy epäluotettavana läpi putken ja todisteessa.

use std::sync::Arc;

use chrono::Duration;
use serde_json::Value;

use familyclaw_core::time::Timestamp;

use crate::approval::{sha256_hex as payload_sha256_hex, Approval, ApprovalLedger};
use crate::audit::{AuditCollector, AuditKind, ExecAuditEvent};
use crate::error::{ActionError, Result};
use crate::executor::{ActionExecutor, ActionRequest, ActionResult, ActionStatus};
use crate::ids::{ActionId, ActionTaskId, ProofBundleId};
use crate::manifest::SkillManifest;
use crate::policy::required_approval;
use crate::proof::{build_proof, ProofBundle, VerificationResult};
use crate::registry::SkillRegistry;
use crate::task::{ActionTask, TaskQueue, TaskStatus};

/// Moduulin valmiusaste — säilytetään, jotta [`crate::all_modules_scaffolded`]
/// kääntyy edelleen muiden moduulien rinnalla.
pub(crate) const SCAFFOLDED: bool = true;

pub mod discord_thread_summary;
pub mod email_triage;
pub mod file_patch;
pub mod fs_read;
pub mod github_issue_draft;

pub use discord_thread_summary::DiscordThreadSummaryMock;
pub use email_triage::EmailTriageMock;
pub use file_patch::FilePatchMock;
pub use fs_read::{FsReadAllowlisted, FsReadConfig};
pub use github_issue_draft::GithubIssueDraftMock;

/// Yhteinen rajapinta taidoille (skills).
///
/// Taito on samalla sekä manifestin tarjoaja ([`Skill::manifest`]) että
/// suorittaja ([`ActionExecutor`]). Tämä yhdistää taidon **kuvauksen** (mitä se
/// saa tehdä, mikä riskiluokka) ja sen **toiminnan** (miten se tuottaa
/// tuloksen) yhteen tyyppiin.
///
/// Tämä on alustan julkinen SPI ulkopuolisille taitojen rakentajille. Aiempi
/// nimi oli `MockSkill`, mikä viestitti virheellisesti "ei tuotantokäyttöön";
/// nimi on nyt `Skill`. [`Skill`] säilyy **deprekoituna aliaksena** yhden
/// julkaisun ajan taaksepäin-yhteensopivuuden vuoksi.
pub trait Skill: ActionExecutor {
    /// Palauttaa taidon manifestin (validoitu, salaisuudeton).
    fn manifest(&self) -> SkillManifest;
}

/// Deprekoitu alias [`Skill`]:lle. Käytä `Skill`:iä uudessa koodissa.
#[deprecated(since = "0.1.0", note = "renamed to `Skill`; use `Skill` instead")]
pub trait MockSkill: Skill {}

// Blanket-impl: jokainen `Skill` on myös `MockSkill` (alias toimii saumattomasti).
#[allow(deprecated)]
impl<T: Skill> MockSkill for T {}

/// Putken lopputulos yhdestä end-to-end-ajosta.
///
/// Kuvaa mihin tilaan tehtävä päätyi ja mitä putki tuotti: mahdollisen
/// todistepaketin, muistiin talletettavan **redaktoidun** yhteenvedon sekä
/// odottaako tehtävä hyväksyntää.
#[derive(Debug, Clone)]
pub struct PipelineOutcome {
    /// Tehtävän tunniste.
    pub task_id: ActionTaskId,
    /// Suoritetun toiminnon tunniste.
    pub action_id: ActionId,
    /// Tehtävän lopputila putken jälkeen.
    pub status: TaskStatus,
    /// Syntynyt todistepaketti (`None` jos tehtävä jäi odottamaan hyväksyntää).
    pub proof: Option<ProofBundle>,
    /// Muistiin talletettava jälki — **vain redaktoitu yhteenveto**, ei raakaa
    /// syötettä eikä salaisuuksia (`None` ennen suoritusta).
    pub memory_record: Option<MemoryRecord>,
    /// Onko tehtävä tällä hetkellä odottamassa ihmisen hyväksyntää.
    pub awaiting_approval: bool,
}

impl PipelineOutcome {
    /// Päätyikö tehtävä onnistuneesti valmiiksi ([`TaskStatus::Done`]).
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.status == TaskStatus::Done
    }

    /// Jäikö tehtävä odottamaan hyväksyntää ([`TaskStatus::NeedsApproval`]).
    #[must_use]
    pub fn needs_approval(&self) -> bool {
        self.status == TaskStatus::NeedsApproval
    }
}

/// Muistiin talletettava jälki yhdestä suorituksesta.
///
/// Tämä on **ainoa** asia jonka putki tarjoaa muistikerrokselle: lyhyt
/// redaktoitu yhteenveto, todistepaketin tunniste ja taint-tila. Raakaa
/// syötettä, payloadia eikä salaisuuksia ei koskaan talleteta tähän.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecord {
    /// Tehtävä jota tämä jälki koskee.
    pub task_id: ActionTaskId,
    /// Lyhyt ihmisluettava yhteenveto (redaktoitu — EI raakoja salaisuuksia).
    pub output_summary: String,
    /// Todistepaketin tunniste, jonka kautta täysi (redaktoitu) jälki löytyy.
    pub proof_bundle_id: ProofBundleId,
    /// Oliko suoritus onnistunut.
    pub succeeded: bool,
    /// Onko jälki peräisin epäluotettavasta lähteestä (taint säilyy).
    pub untrusted: bool,
}

/// Toimintopinon putki: ajaa tehtävän rekisteristä todisteeseen asti.
///
/// Putki on omisteinen yhdelle ajolle: se kantaa rekisterin, jonon,
/// hyväksyntärekisterin ja audit-keräimen. Suorittajat annetaan ajokohtaisesti
/// ([`Pipeline::run`]). Aikaleima injektoidaan jokaiseen kutsuun — kelloa ei
/// lueta putken logiikan sisällä.
#[derive(Debug, Default)]
pub struct Pipeline {
    /// Taitojen rekisteri (validoidut manifestit).
    registry: SkillRegistry,
    /// Tehtäväjono (tilakone).
    queue: TaskQueue,
    /// Hyväksyntärekisteri (human-in-the-loop).
    ledger: ApprovalLedger,
    /// Suorituspinon audit-keräin.
    audit: AuditCollector,
}

impl Pipeline {
    /// Luo uuden tyhjän putken.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rekisteröi taidon manifestin putken rekisteriin.
    ///
    /// # Errors
    /// Palauttaa manifestin validoinnin virheen tai duplikaattivirheen
    /// ([`SkillRegistry::register`]).
    pub fn register_skill<S: Skill>(&mut self, skill: &S) -> Result<()> {
        self.registry.register(skill.manifest())
    }

    /// Pääsy rekisteriin (vain luku).
    #[must_use]
    pub fn registry(&self) -> &SkillRegistry {
        &self.registry
    }

    /// Pääsy tehtäväjonoon (vain luku).
    #[must_use]
    pub fn queue(&self) -> &TaskQueue {
        &self.queue
    }

    /// Pääsy audit-keräimeen (vain luku).
    #[must_use]
    pub fn audit(&self) -> &AuditCollector {
        &self.audit
    }

    /// Pääsy hyväksyntärekisteriin (vain luku).
    #[must_use]
    pub fn ledger(&self) -> &ApprovalLedger {
        &self.ledger
    }

    /// Ajaa tehtävän koko putken läpi: plan → policy → (approval) → execute →
    /// verify → proof → remember → report.
    ///
    /// Vaiheet:
    /// 1. **Plan** — tehtävä lisätään jonoon ja siirretään `Planned → Ready`.
    /// 2. **Policy** — hyväksyntävaatimus johdetaan **manifestista**
    ///    ([`required_approval`]), EI payloadista. Jos hyväksyntä vaaditaan,
    ///    tehtävä siirtyy `Ready → Running → NeedsApproval` ja putki palaa
    ///    ([`PipelineOutcome::awaiting_approval`] = `true`) ilman suoritusta.
    /// 3. **Execute** — auto-run-tehtävälle (tai jo hyväksytylle) suoritus
    ///    ajetaan annetulla suorittajalla.
    /// 4. **Verify** — tulos tarkistetaan (onnistuiko, säilyikö taint).
    /// 5. **Proof** — koostetaan redaktoitu todistepaketti.
    /// 6. **Remember** — muodostetaan [`MemoryRecord`] (vain redaktoitu yhteenveto).
    /// 7. **Report** — tehtävä siirtyy `Running → Done` (tai `Failed`).
    ///
    /// # Errors
    /// - [`ActionError::UnknownSkill`] jos taitoa ei ole rekisterissä.
    /// - Jonon tilakone- tai validointivirheet.
    /// - Suorittajan tai todisteen rakennuksen virheet.
    pub async fn run<E: ActionExecutor + ?Sized>(
        &self,
        executor: &E,
        task: ActionTask,
        now: Timestamp,
    ) -> Result<PipelineOutcome> {
        self.run_with_input_taint(executor, task, now, false).await
    }

    /// Kuten [`Pipeline::run`], mutta merkitsee tehtävän syötteen
    /// epäluotettavaksi (`input_untrusted`).
    ///
    /// Käytä tätä kun tehtävän payload on rakennettu epäluotettavasta lähteestä
    /// (esim. MCP-työkalun tuloste). Taint **propagoituu** suorituksen läpi:
    /// vaikka suorittaja merkitsisi oman tulosteensa luotetuksi, MCP-lähteinen
    /// taint säilyy tuloksessa, todisteessa ja muistijäljessä (ei laundering).
    ///
    /// # Errors
    /// Sama kuin [`Pipeline::run`].
    pub async fn run_with_input_taint<E: ActionExecutor + ?Sized>(
        &self,
        executor: &E,
        task: ActionTask,
        now: Timestamp,
        input_untrusted: bool,
    ) -> Result<PipelineOutcome> {
        let action_id = ActionId::new();

        // --- Plan: tarkista että taito on olemassa, lisää jonoon, tee Ready. ---
        let manifest = self
            .registry
            .get(&task.skill_id)
            .ok_or_else(|| ActionError::UnknownSkill(task.skill_id.to_string()))?
            .clone();

        let task_id = task.id;
        let payload = task.payload.clone();
        self.queue.submit(task).await?;
        self.queue
            .transition(task_id, TaskStatus::Ready, now)
            .await?;

        // --- Policy: vaatimus johdetaan MANIFESTISTA, ei payloadista. ---
        let requirement = required_approval(manifest.risk, manifest.approval_policy);

        // Siirrä ajoon. Jos hyväksyntä vaaditaan, pysähdy NeedsApproval-tilaan.
        self.queue
            .transition(task_id, TaskStatus::Running, now)
            .await?;

        if requirement.requires_approval() {
            self.queue
                .transition(task_id, TaskStatus::NeedsApproval, now)
                .await?;
            self.audit.record(ExecAuditEvent::new(
                AuditKind::PolicyDenied,
                action_id,
                now,
                "policy requires human approval before execution",
            ));
            return Ok(PipelineOutcome {
                task_id,
                action_id,
                status: TaskStatus::NeedsApproval,
                proof: None,
                memory_record: None,
                awaiting_approval: true,
            });
        }

        // --- Execute + verify + proof + remember + report (auto-run-polku). ---
        self.execute_and_finalize(executor, task_id, action_id, &payload, now, input_untrusted)
            .await
    }

    /// Jatkaa hyväksyntää odottaneen tehtävän: kuluttaa hyväksynnän ja ajaa
    /// suorituksen loppuun (`NeedsApproval → Running → Done/Failed`).
    ///
    /// Hyväksyntä kulutetaan tehtävän payloadia vasten (payload-sidonta), joten
    /// muutettu payload ei voi käyttää myönnettyä hyväksyntää.
    ///
    /// # Errors
    /// - [`ActionError::NotFound`] jos tehtävää ei ole jonossa.
    /// - Hyväksynnän kulutuksen virheet ([`ApprovalLedger::consume`]).
    /// - Jonon tilakone- tai todistevirheet.
    pub async fn run_after_approval<E: ActionExecutor + ?Sized>(
        &mut self,
        executor: &E,
        task_id: ActionTaskId,
        approval: &Approval,
        now: Timestamp,
    ) -> Result<PipelineOutcome> {
        self.run_after_approval_with_input_taint(executor, task_id, approval, now, false)
            .await
    }

    /// Kuten [`Pipeline::run_after_approval`], mutta merkitsee syötteen
    /// epäluotettavaksi (`input_untrusted`) jolloin MCP-lähteinen taint säilyy
    /// suorituksen läpi todisteeseen asti.
    ///
    /// # Errors
    /// Sama kuin [`Pipeline::run_after_approval`].
    pub async fn run_after_approval_with_input_taint<E: ActionExecutor + ?Sized>(
        &mut self,
        executor: &E,
        task_id: ActionTaskId,
        approval: &Approval,
        now: Timestamp,
        input_untrusted: bool,
    ) -> Result<PipelineOutcome> {
        let task = self
            .queue
            .get(task_id)
            .await
            .ok_or_else(|| ActionError::NotFound(format!("tehtävää {task_id} ei löydy")))?;
        let payload = task.payload.clone();

        // Kuluta hyväksyntä tehtävän payloadia vasten (kertakäyttö + sidonta).
        let payload_bytes = serde_json::to_vec(&payload)
            .map_err(|e| ActionError::Proof(format!("payload serialize failed: {e}")))?;
        self.ledger.consume(approval.id, &payload_bytes, now)?;

        // NeedsApproval → Running, sitten suoritus loppuun.
        self.queue
            .transition(task_id, TaskStatus::Running, now)
            .await?;
        self.execute_and_finalize(
            executor,
            task_id,
            approval.action_id,
            &payload,
            now,
            input_untrusted,
        )
        .await
    }

    /// Myöntää hyväksynnän tehtävän payloadiin sidottuna.
    ///
    /// Palauttaa myönnetyn [`Approval`]:n, jonka voi antaa
    /// [`Pipeline::run_after_approval`]:lle. Payload haetaan jonosta ja sidotaan
    /// SHA-256-tiivisteenä.
    ///
    /// # Errors
    /// - [`ActionError::NotFound`] jos tehtävää ei ole jonossa.
    /// - Payloadin sarjallistuksen virhe.
    pub fn grant_approval(
        &mut self,
        action_id: ActionId,
        payload: &Value,
        now: Timestamp,
        ttl: Duration,
    ) -> Result<Approval> {
        let payload_bytes = serde_json::to_vec(payload)
            .map_err(|e| ActionError::Proof(format!("payload serialize failed: {e}")))?;
        let hash = payload_sha256_hex(&payload_bytes);
        Ok(self.ledger.grant(action_id, hash, now, ttl))
    }

    /// Suoritus + verify + proof + remember + report -loppuosa (jaettu auto-run-
    /// ja hyväksyntäpolun kesken).
    async fn execute_and_finalize<E: ActionExecutor + ?Sized>(
        &self,
        executor: &E,
        task_id: ActionTaskId,
        action_id: ActionId,
        payload: &Value,
        now: Timestamp,
        input_untrusted: bool,
    ) -> Result<PipelineOutcome> {
        let task = self
            .queue
            .get(task_id)
            .await
            .ok_or_else(|| ActionError::NotFound(format!("tehtävää {task_id} ei löydy")))?;

        self.audit.record(ExecAuditEvent::new(
            AuditKind::ActionStarted,
            action_id,
            now,
            "action execution started",
        ));

        // --- Execute ---
        // Pyyntö kantaa syötteen taint-tilan, jotta suorittaja ei voi pestä
        // epäluotettavaa (esim. MCP-lähteistä) syötettä puhtaaksi.
        let request = ActionRequest::new(action_id, task.skill_id, task_id, payload.clone(), now)
            .with_input_taint(input_untrusted);
        // Taint propagoituu monotonisesti: syötteen taint pakottaa tulosteen
        // taintatuksi, vaikka suorittaja merkitsisi oman tulosteensa luotetuksi.
        let result = executor
            .execute(request.clone())
            .await?
            .propagate_input_taint(input_untrusted);

        // --- Verify ---
        let verification = verify_result(&result);

        // Audit: onnistuminen/epäonnistuminen + mahdollinen taint-merkintä.
        let kind = if result.status.is_success() {
            AuditKind::ActionSucceeded
        } else {
            AuditKind::ActionFailed
        };
        self.audit.record(ExecAuditEvent::new(
            kind,
            action_id,
            now,
            "action execution finished",
        ));
        if result.untrusted {
            self.audit.record(ExecAuditEvent::new(
                AuditKind::TaintMarked,
                action_id,
                now,
                "result output marked untrusted (taint preserved)",
            ));
        }

        // --- Proof (redaktoitu) ---
        let audit_ids = self
            .audit
            .list()
            .iter()
            .filter(|e| e.action_id == action_id)
            .map(|e| e.id)
            .collect();
        let proof = build_proof(&request, &result, audit_ids, verification)?;
        if proof.redaction.any_redacted() {
            self.audit.record(ExecAuditEvent::new(
                AuditKind::RedactionApplied,
                action_id,
                now,
                "secret-looking values redacted in proof",
            ));
        }

        // --- Remember (vain redaktoitu yhteenveto) ---
        let memory_record = MemoryRecord {
            task_id,
            output_summary: result.output_summary.clone(),
            proof_bundle_id: proof.id,
            succeeded: result.status.is_success(),
            untrusted: result.untrusted,
        };

        // --- Report (tilan päätös) ---
        let final_status = if result.status == ActionStatus::Succeeded {
            TaskStatus::Done
        } else {
            TaskStatus::Failed
        };
        self.queue.transition(task_id, final_status, now).await?;

        Ok(PipelineOutcome {
            task_id,
            action_id,
            status: final_status,
            proof: Some(proof),
            memory_record: Some(memory_record),
            awaiting_approval: false,
        })
    }
}

/// Verify-vaihe: tarkistaa tuloksen kelpoisuuden jälkiehtoja vasten.
///
/// Tarkistukset:
/// - `status_succeeded` — toiminnon lopputila on onnistunut,
/// - `taint_preserved` — jos tuloste on epäluotettava, se merkitään huomioksi
///   (taint ei katoa verifioinnissa).
fn verify_result(result: &ActionResult) -> VerificationResult {
    let mut checks = vec!["status_checked".to_string()];
    if result.untrusted {
        checks.push("taint_preserved".to_string());
    }
    if result.status.is_success() {
        checks.push("status_succeeded".to_string());
        VerificationResult::passed(checks, "post-conditions satisfied")
    } else {
        checks.push("status_failed".to_string());
        VerificationResult::failed(checks, "action did not succeed")
    }
}

/// Apuri: muuntaa taidon jaetuksi [`Arc`]-viitteeksi, jotta sama taito voi
/// toimia samanaikaisesti sekä rekisterissä että suorittajana.
///
/// KERROS A -mukavuusfunktio testeille ja arvioinneille.
#[must_use]
pub fn shared<S: Skill + 'static>(skill: S) -> Arc<S> {
    Arc::new(skill)
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time::from_unix_secs;
    use serde_json::json;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    #[test]
    fn verify_passes_on_success() {
        let r = ActionResult::success("ok", json!({}), at(1));
        let v = verify_result(&r);
        assert!(v.verified);
        assert!(v.checks.iter().any(|c| c == "status_succeeded"));
    }

    #[test]
    fn verify_fails_on_failure() {
        let r = ActionResult::failure("nope", at(1));
        let v = verify_result(&r);
        assert!(!v.verified);
    }

    #[test]
    fn verify_notes_taint() {
        let r = ActionResult::success("ok", json!({}), at(1));
        assert!(r.untrusted, "success is untrusted by default");
        let v = verify_result(&r);
        assert!(v.checks.iter().any(|c| c == "taint_preserved"));
    }

    #[test]
    fn pipeline_outcome_helpers() {
        let done = PipelineOutcome {
            task_id: ActionTaskId::new(),
            action_id: ActionId::new(),
            status: TaskStatus::Done,
            proof: None,
            memory_record: None,
            awaiting_approval: false,
        };
        assert!(done.is_done());
        assert!(!done.needs_approval());
    }

    #[test]
    fn shared_wraps_skill() {
        let s = shared(GithubIssueDraftMock::new());
        // Manifestin haku jaetun viitteen kautta toimii.
        assert_eq!(s.manifest().name, "github_issue_draft");
    }

    /// Vakooja-suorittaja: kirjaa montako kertaa sitä kutsuttiin, jotta
    /// "fail-closed" voidaan todistaa sivuvaikutuksen puuttumisena — ei pelkkänä
    /// virheenä. Onnistuu aina kun sitä kutsutaan.
    #[derive(Debug, Default)]
    struct SpyExecutor {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ActionExecutor for SpyExecutor {
        async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ActionResult::success(
                "spy ran",
                request.payload,
                request.now,
            ))
        }
    }

    impl SpyExecutor {
        fn call_count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// ADVERSARIAL: hyväksyntä on PAYLOAD-SIDOTTU end-to-end putkessa.
    ///
    /// Skenaario: vaarallinen (write-external) tehtävä pysähtyy hyväksyntään.
    /// Ihminen hyväksyy payloadin A (se mitä hänelle näytettiin). Hyökkääjä
    /// yrittää jatkaa suoritusta hyväksynnällä joka myönnettiin ERI payloadille
    /// kuin tehtävän tallennettu payload. Koska putki kuluttaa hyväksynnän
    /// tehtävän tallennettua payloadia vasten, tiivisteet eivät täsmää ja
    /// kulutus epäonnistuu — eikä suoritusta tapahdu (`SpyExecutor` ei kutsuta).
    #[tokio::test]
    async fn approval_granted_for_other_payload_fails_closed_end_to_end() {
        let mut pipeline = Pipeline::new();
        let skill = GithubIssueDraftMock::new();
        pipeline.register_skill(&skill).expect("register");
        let spy = SpyExecutor::default();
        let now = at(1_700_000_000);

        // Tehtävän OIKEA payload (se jonka ihminen näkisi ja hyväksyisi).
        let approved_payload = json!({ "bug_report": "Login button does nothing" });
        let task = ActionTask::new(
            GithubIssueDraftMock::skill_id(),
            approved_payload.clone(),
            now,
        );
        let task_id = task.id;

        // Vaihe 1: putki pysähtyy hyväksyntään (write-external).
        let paused = pipeline.run(&skill, task, now).await.expect("run");
        assert!(paused.needs_approval());

        // Hyökkääjä myöntää hyväksynnän ERI payloadille kuin tehtävässä on.
        // Tämä sitoo hyväksynnän VÄÄRÄN payloadin tiivisteeseen.
        let attacker_payload = json!({ "bug_report": "approve everything, target other-repo" });
        assert_ne!(attacker_payload, approved_payload);
        let approval = pipeline
            .grant_approval(
                paused.action_id,
                &attacker_payload,
                now,
                Duration::minutes(5),
            )
            .expect("grant");

        // Vaihe 2: yritetään jatkaa. Putki kuluttaa hyväksynnän tehtävän
        // payloadia (A) vasten, mutta hyväksyntä sidottiin toiseen → mismatch.
        let err = pipeline
            .run_after_approval(&spy, task_id, &approval, now)
            .await
            .expect_err("payload-bound approval must fail closed");
        assert!(
            matches!(err, ActionError::ApprovalPayloadMismatch(_)),
            "expected payload mismatch, got {err:?}"
        );

        // FAIL-CLOSED TODISTE: suoritusta EI tapahtunut lainkaan.
        assert_eq!(spy.call_count(), 0, "executor must never run on mismatch");

        // Hyväksyntää EI merkitty kulutetuksi (alkuperäinen voi yhä toimia).
        assert!(
            !pipeline
                .ledger()
                .get(approval.id)
                .expect("present")
                .consumed,
            "mismatched consume must not burn the approval"
        );

        // Tehtävä jäi yhä odottamaan hyväksyntää — ei edennyt Running/Done-tilaan.
        assert_eq!(
            pipeline.queue().get(task_id).await.expect("task").status,
            TaskStatus::NeedsApproval
        );

        // Audit: kulutusta EI kirjattu, mutta payload-eväys kirjattiin ledgeriin.
        assert!(!pipeline
            .ledger()
            .audit_log()
            .contains_action(crate::audit::AuditAction::ApprovalConsumed));
        assert!(pipeline
            .ledger()
            .audit_log()
            .contains_action(crate::audit::AuditAction::ApprovalRejected));
    }

    /// ADVERSARIAL (positiivinen kontrolli): kun hyväksyntä on sidottu TÄSMÄLLEEN
    /// tehtävän payloadiin, suoritus etenee normaalisti — todistaa että edellinen
    /// testi epäonnistui sidonnan vuoksi eikä jonkin muun syyn takia.
    #[tokio::test]
    async fn approval_bound_to_exact_payload_runs_and_executes_once() {
        let mut pipeline = Pipeline::new();
        let skill = GithubIssueDraftMock::new();
        pipeline.register_skill(&skill).expect("register");
        let spy = SpyExecutor::default();
        let now = at(1_700_000_000);

        let payload = json!({ "bug_report": "Login button does nothing" });
        let task = ActionTask::new(GithubIssueDraftMock::skill_id(), payload.clone(), now);
        let task_id = task.id;

        let paused = pipeline.run(&skill, task, now).await.expect("run");
        assert!(paused.needs_approval());

        // Hyväksyntä sidotaan SAMAAN payloadiin kuin tehtävässä.
        let approval = pipeline
            .grant_approval(paused.action_id, &payload, now, Duration::minutes(5))
            .expect("grant");
        let done = pipeline
            .run_after_approval(&spy, task_id, &approval, now)
            .await
            .expect("matching payload resumes");

        assert!(done.is_done());
        assert_eq!(spy.call_count(), 1, "executor must run exactly once");
        assert!(
            pipeline
                .ledger()
                .get(approval.id)
                .expect("present")
                .consumed
        );
    }
}
