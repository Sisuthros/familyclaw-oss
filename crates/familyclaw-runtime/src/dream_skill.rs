//! Uni-sykli ajastettavana taitona ([`DreamSkill`]) — Phase 4, D5.
//!
//! Tämä kääräisee [`familyclaw_dream::DreamCycle`]n
//! [`familyclaw_actions`]-taidoksi, jotta [`familyclaw_scheduler`] voi ajaa sen
//! ajastettuna tehtävänä (`submit_task_idempotent`:n läpi) sen sijaan että
//! runtime pitäisi käsin koodattua `tokio::sleep`-silmukkaa. Hyödyt:
//! - **havainnoitavuus:** ajo kulkee toiminto-ajoympäristön kautta → turn-audit
//!   + mittarit näkevät sen kuten minkä tahansa työkalukutsun;
//! - **idempotenssi:** ajastimen deterministinen avain estää kaksoisajon
//!   kaatumis-replayn yli;
//! - **yhtenäisyys:** yksi ajastinmekanismi kaikille proaktiivisille tehtäthe operator.
//!
//! Taito asuu **runtimessa** (ei `familyclaw-actions`-Kerros-A-ytimessä), koska
//! se tarvitsee `familyclaw-dream` + `familyclaw-memory` -kahvat;
//! `ActionRuntime::register_skill` on julkinen, joten ulkoinen taito voidaan
//! rekisteröidä ilman että ydin-crate paisuu näillä riippuvuuksilla.

use std::sync::Arc;

use async_trait::async_trait;
use familyclaw_actions::manifest::SkillManifest;
use familyclaw_actions::policy::{ActionRisk, ApprovalPolicy, SkillPermission};
use familyclaw_actions::{ActionExecutor, ActionRequest, ActionResult, Result, Skill, SkillId};
use familyclaw_dream::DreamCycle;
use familyclaw_durable::Journal;
use familyclaw_memory::MemoryStore;
use serde_json::json;

/// Taidon kiinteä tunniste. Vakio-UUID, jotta ajastimen idempotenssiavain on
/// vakaa prosessien yli (ASCII "dreamcyc" -tavut täytteenä).
const DREAM_SKILL_UUID: uuid::Uuid = uuid::uuid!("d4ea3c1c-0000-4000-8000-647265616d63");

/// Uni-sykli ajastettavana taitona.
///
/// Kantaa jaetut [`MemoryStore`]- ja [`Journal`]-kahvat (samat joita runtime
/// käyttää), jotta [`execute`](ActionExecutor::execute) voi ajaa täyden
/// uni-syklin ilman payload-riippuvuutta — syöte on tyhjä objekti `{}`.
pub struct DreamSkill {
    store: Arc<dyn MemoryStore + Send + Sync>,
    journal: Arc<dyn Journal + Send + Sync>,
}

impl DreamSkill {
    /// Rakentaa taidon jaetuilla muisti- ja journal-kahvoilla.
    #[must_use]
    pub fn new(
        store: Arc<dyn MemoryStore + Send + Sync>,
        journal: Arc<dyn Journal + Send + Sync>,
    ) -> Self {
        Self { store, journal }
    }

    /// Taidon kiinteä tunniste (ajastimen tehtävä viittaa tähän).
    #[must_use]
    pub fn skill_id() -> SkillId {
        SkillId::from_uuid(DREAM_SKILL_UUID)
    }
}

#[async_trait]
impl ActionExecutor for DreamSkill {
    async fn execute(&self, request: ActionRequest) -> Result<ActionResult> {
        // Aja yksi uni-sykli injektoidulla kellolla (`request.now`) — sama
        // aikaleima jonka ajastin/runtime journaloi → deterministinen.
        let store: &(dyn MemoryStore + Send + Sync) = &*self.store;
        let journal: &(dyn Journal + Send + Sync) = &*self.journal;
        let cycle = DreamCycle::new(store);
        match cycle.run(journal, request.now).await {
            Ok(report) => {
                let output = json!({
                    "scanned": report.scanned,
                    "merged": report.merged,
                    "dropped": report.dropped,
                    "strengthened": report.strengthened,
                    "archived": report.archived,
                    "dates_absolutized": report.dates_absolutized,
                });
                Ok(ActionResult::success(
                    format!(
                        "dream cycle: scanned={} merged={} dropped={} strengthened={} archived={}",
                        report.scanned,
                        report.merged,
                        report.dropped,
                        report.strengthened,
                        report.archived
                    ),
                    output,
                    request.now,
                ))
            }
            Err(e) => Ok(ActionResult::failure(
                format!("dream cycle failed: {e}"),
                request.now,
            )),
        }
    }
}

impl Skill for DreamSkill {
    fn manifest(&self) -> SkillManifest {
        SkillManifest {
            id: Self::skill_id(),
            name: "dream_cycle".to_string(),
            version: "1.0.0".to_string(),
            description: "Ajaa yhden muistin konsolidaatio-syklin (merge/drop/strengthen/archive) \
                          Eternal Thread -muistille. Sisäinen ylläpito; ei käyttäjäsyötettä."
                .to_string(),
            permissions: vec![SkillPermission::WriteLocalFiles],
            // Muokkaa paikallista muistia → WriteLocal. `RequireApproval`
            // yhdessä WriteLocal-riskin kanssa AJAA AUTOMAATTISESTI (ks.
            // `policy.rs`: WriteLocal + RequireApproval → AutoRun) — sisäinen
            // ylläpitotehtävä ei vaadi ihmishyväksyntää per ajo, mutta ei
            // myöskään ole pelkkä luku (AutoIfReadOnly olisi väärä).
            risk: ActionRisk::WriteLocal,
            approval_policy: ApprovalPolicy::RequireApproval,
            input_hint: Some("{} (ei syötettä)".to_string()),
            output_hint: Some("uni-raportin laskurit (scanned/merged/dropped/...)".to_string()),
            input_schema: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_durable::InMemoryJournal;
    use familyclaw_memory::{LocalJsonStore, Memory, MemoryStore};

    fn skill() -> DreamSkill {
        let store: Arc<dyn MemoryStore + Send + Sync> = Arc::new(LocalJsonStore::in_memory());
        let journal: Arc<dyn Journal + Send + Sync> = Arc::new(InMemoryJournal::new());
        DreamSkill::new(store, journal)
    }

    #[test]
    fn manifest_is_write_local_autorun_and_generic() {
        let m = skill().manifest();
        assert_eq!(m.name, "dream_cycle");
        assert_eq!(m.risk, ActionRisk::WriteLocal);
        assert_eq!(m.approval_policy, ApprovalPolicy::RequireApproval);
        assert_eq!(m.permissions, vec![SkillPermission::WriteLocalFiles]);
        assert_eq!(m.id, DreamSkill::skill_id());
        // Layer B: nimi/kuvaus geneerisiä, ei perheen nimiä. Kielletyt nimet
        // rakennetaan ROT13:sta, jotta tämä TESTI ei itse sisällä Kerros B
        // -nimiä literaaleina (muuten scripts/audit-layer-b.sh napsahtaisi tähän
        // testitiedostoon vaikka tuotantokoodi on puhdas).
        let blob = format!("{} {}", m.name, m.description).to_lowercase();
        let forbidden_rot13 = ["yhzra", "yhzvan", "cubgba", "cevfzn", "nheben"];
        for enc in forbidden_rot13 {
            let frag: String = enc
                .chars()
                .map(|c| (((c as u8 - b'a' + 13) % 26) + b'a') as char)
                .collect();
            assert!(!blob.contains(&frag), "Layer B -vuoto manifestissa");
        }
    }

    #[tokio::test]
    async fn execute_runs_a_cycle_on_empty_store() {
        let store: Arc<dyn MemoryStore + Send + Sync> = Arc::new(LocalJsonStore::in_memory());
        let journal: Arc<dyn Journal + Send + Sync> = Arc::new(InMemoryJournal::new());
        let s = DreamSkill::new(Arc::clone(&store), journal);
        let now = familyclaw_core::time::from_unix_secs(1_717_000_000).expect("clock");
        let req = ActionRequest::new(
            familyclaw_actions::ActionId::new(),
            DreamSkill::skill_id(),
            familyclaw_actions::ActionTaskId::new(),
            json!({}),
            now,
        );
        let result = s.execute(req).await.expect("execute");
        // Tyhjä muisti → onnistuu, skannattu 0.
        assert!(matches!(
            result.status,
            familyclaw_actions::ActionStatus::Succeeded
        ));
        assert!(result.output_summary.contains("scanned=0"));
    }

    #[tokio::test]
    async fn execute_consolidates_real_memories() {
        let store: Arc<dyn MemoryStore + Send + Sync> = Arc::new(LocalJsonStore::in_memory());
        let now = familyclaw_core::time::from_unix_secs(1_717_000_000).expect("clock");
        store
            .add(Memory::builder("muisto yksi").created_at(now).build())
            .await
            .expect("add");
        let journal: Arc<dyn Journal + Send + Sync> = Arc::new(InMemoryJournal::new());
        let s = DreamSkill::new(Arc::clone(&store), journal);
        let req = ActionRequest::new(
            familyclaw_actions::ActionId::new(),
            DreamSkill::skill_id(),
            familyclaw_actions::ActionTaskId::new(),
            json!({}),
            now,
        );
        let result = s.execute(req).await.expect("execute");
        assert!(matches!(
            result.status,
            familyclaw_actions::ActionStatus::Succeeded
        ));
        // Yksi muisto skannattiin.
        assert!(result.output_summary.contains("scanned=1"));
    }
}
