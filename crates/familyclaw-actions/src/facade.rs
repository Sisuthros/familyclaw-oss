//! Operaattoripinta toimintoajoympäristölle (KERROS A).
//!
//! [`ActionRuntime`] on ohut julkisivu (facade), joka sitoo yhteen koko
//! toimintopinon — rekisterin, jonon, hyväksyntärekisterin, suorittajat,
//! todisteet ja audit-keräimen — yhden tyypin taakse, jotta operaattorin
//! työkalut (esim. komentorivibinääri `familyclaw-actions-cli`) voivat olla
//! pelkkiä kuoria. Julkisivu tarjoaa juuri ne operaatiot jotka operaattori
//! tarvitsee:
//!
//! ```text
//! list-skills   → rekisteröidyt taidot + riskiluokka (ei salaisuuksia)
//! submit-task   → lähetä tehtävä, aja putki, palauta tehtävän tunniste
//! approve       → kuluta/merkitse hyväksyntä → jatka suoritus loppuun
//! status        → tehtävän tila
//! proof         → redaktoitu todistepaketti (haettavissa tunnisteella)
//! ```
//!
//! ## Turvaperiaatteet (samat kuin putkella)
//! - **Käytäntö johdetaan AINA manifestista**, ei tehtävän payloadista.
//! - **Vain redaktoidut todisteet** ([`crate::proof`]) tallennetaan ja
//!   palautetaan — raakaa payloadia tai salaisuuksia ei koskaan paljasteta.
//! - **Hyväksyntä on payload-sidottu ja kertakäyttöinen**; muutettu payload ei
//!   voi käyttää myönnettyä hyväksyntää.
//! - **Determinismi:** aikaleima injektoidaan jokaiseen kutsuun — kelloa ei
//!   lueta logiikan sisällä.
//!
//! ## OSS-raja (KERROS A)
//! Julkisivu rekisteröi vain geneerisiä **mock-taitoja** ([`crate::skills`]) —
//! ei oikeita providereita, sieluja, avaimia eikä henkilökohtaisia polkuja.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Duration;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use familyclaw_core::time::Timestamp;

use crate::approval::Approval;
use crate::error::{ActionError, Result};
use crate::executor::ActionExecutor;
use crate::ids::{ActionTaskId, ApprovalId, SkillId};
use crate::mcp::McpToolDescriptor;
use crate::policy::{ActionRisk, SkillPermission};
use crate::proof::ProofBundle;
use crate::skills::{
    DiscordThreadSummaryMock, EmailTriageMock, FilePatchMock, FsReadAllowlisted,
    GithubIssueDraftMock, Pipeline, Skill,
};
use crate::task::{ActionTask, TaskStatus};

/// Moduulin valmiusaste — säilytetään, jotta [`crate::all_modules_scaffolded`]
/// kääntyy edelleen muiden moduulien rinnalla.
pub(crate) const SCAFFOLDED: bool = true;

/// Hyväksyntäpyynnön oletus-TTL kun operaattori myöntää hyväksynnän
/// (`submit-task` jättää tehtävän odottamaan; hyväksyntä on voimassa tämän ajan).
const DEFAULT_APPROVAL_TTL_MINUTES: i64 = 60;

/// Yhden taidon tiivistetty kuvaus operaattorin luettelointia varten.
///
/// Sisältää vain julkiset, salaisuudettomat kentät — tunniste, nimi, versio ja
/// riskiluokka — jotta tulosteen voi näyttää suoraan operaattorille.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSummary {
    /// Taidon tunniste rekisterissä.
    pub id: SkillId,
    /// Ihmisluettava nimi.
    pub name: String,
    /// Versiomerkkijono.
    pub version: String,
    /// Toiminnon riskiluokka (ohjaa hyväksyntävaatimusta).
    pub risk: ActionRisk,
    /// Vaatiiko tämä taito ihmisen hyväksynnän ennen suoritusta.
    pub requires_approval: bool,
}

/// `submit-task`-operaation lopputulos operaattorille.
///
/// Kertoo lähetetyn tehtävän tunnisteen, tehtävän tilan putken jälkeen sekä —
/// jos tehtävä pysähtyi odottamaan ihmisen hyväksyntää — myönnetyn
/// hyväksynnän tunnisteen jolla suorituksen voi jatkaa (`approve`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitOutcome {
    /// Lähetetyn tehtävän tunniste.
    pub task_id: ActionTaskId,
    /// Tehtävän tila putken ensimmäisen ajon jälkeen.
    pub status: TaskStatus,
    /// Hyväksynnän tunniste jolla suorituksen voi jatkaa, jos tehtävä jäi
    /// odottamaan hyväksyntää (`None` jos tehtävä eteni jo loppuun).
    pub pending_approval: Option<ApprovalId>,
}

impl SubmitOutcome {
    /// Jäikö tehtävä odottamaan ihmisen hyväksyntää.
    #[must_use]
    pub const fn awaiting_approval(&self) -> bool {
        self.pending_approval.is_some()
    }
}

/// Yhden odottavan hyväksynnän tiivistelmä operaattorin näytettäväksi.
///
/// Salaisuudeton: viittaa vain tunnisteilla siihen mitä hyväksyntä koskee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    /// Hyväksynnän tunniste (`approve <id>` jatkaa suorituksen).
    pub approval_id: ApprovalId,
    /// Tehtävä jota hyväksyntä koskee.
    pub task_id: ActionTaskId,
}

/// Odottavan hyväksynnän sisäinen kirjaus (julkisivun oma tila).
///
/// Säilyttää tehtävän tunnisteen ja itse hyväksynnän, jotta `approve` voi
/// kuluttaa sen tehtävän tallennettua payloadia vasten.
#[derive(Debug, Clone)]
struct PendingEntry {
    /// Tehtävä jota hyväksyntä koskee.
    task_id: ActionTaskId,
    /// Myönnetty hyväksyntä (payload-sidottu).
    approval: Approval,
}

/// Toimintoajoympäristön julkisivu: ohut operaattoripinta koko putken päälle.
///
/// Omistaa putken ([`Pipeline`]), taitojen suorittajat, syntyneet todisteet ja
/// odottavat hyväksynnät. Operaattorin työkalu kutsuu vain tämän julkisia
/// metodeja eikä koske putken sisäosiin.
///
/// Aikaleima injektoidaan jokaiseen kutsuun, jotta käyttäytyminen on
/// deterministinen ja testattava.
///
/// [`Debug`] toteutetaan käsin: suorittajat ([`ActionExecutor`]-trait-objektit)
/// eivät toteuta [`Debug`]:ia, joten niistä tulostetaan vain lukumäärä.
#[derive(Default)]
pub struct ActionRuntime {
    /// Koko toimintopinon putki (rekisteri + jono + ledger + audit).
    pipeline: Pipeline,
    /// Taidon tunniste → suorittaja, suoritusta varten.
    executors: HashMap<SkillId, Arc<dyn ActionExecutor>>,
    /// Tehtävän tunniste → syntynyt redaktoitu todistepaketti.
    proofs: HashMap<ActionTaskId, ProofBundle>,
    /// Odottavat hyväksynnät tunnisteen mukaan.
    pending: HashMap<ApprovalId, PendingEntry>,
}

impl std::fmt::Debug for ActionRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionRuntime")
            .field("pipeline", &self.pipeline)
            .field("executor_count", &self.executors.len())
            .field("proofs", &self.proofs.len())
            .field("pending", &self.pending.len())
            .finish()
    }
}

impl ActionRuntime {
    /// Luo uuden tyhjän ajoympäristön ilman rekisteröityjä taitoja.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Luo ajoympäristön jossa kaikki viisi KERROS A -taitoa on rekisteröity
    /// valmiiksi.
    ///
    /// Tämä on operaattorin oletuskokoonpano: [`EmailTriageMock`],
    /// [`GithubIssueDraftMock`], [`DiscordThreadSummaryMock`], [`FilePatchMock`]
    /// ja lippulaiva [`FsReadAllowlisted`].
    ///
    /// [`FsReadAllowlisted`] rekisteröidään **tyhjällä allowlistilla**
    /// (fail-closed): se on luettelossa ja julkaistaan MCP-työkaluna, mutta
    /// hylkää kaikki polut kunnes operaattori antaa allowlistin
    /// ([`FsReadAllowlisted::with_config`]) ja rekisteröi sen
    /// [`ActionRuntime::register_skill`]:llä. Näin oletuskokoonpano ei kovakoodaa
    /// yhtään polkua ja pysyy geneerisenä.
    ///
    /// # Errors
    /// Palauttaa manifestin validoinnin tai duplikaattirekisteröinnin virheen,
    /// jos jokin sisäänrakennettu taito on virheellinen (ei pitäisi tapahtua).
    pub fn with_default_skills() -> Result<Self> {
        let mut runtime = Self::new();
        runtime.register_skill(EmailTriageMock::new())?;
        runtime.register_skill(GithubIssueDraftMock::new())?;
        runtime.register_skill(DiscordThreadSummaryMock::new())?;
        runtime.register_skill(FilePatchMock::new())?;
        runtime.register_skill(FsReadAllowlisted::new())?;
        Ok(runtime)
    }

    /// Rekisteröi taidon sekä putken rekisteriin (manifesti) että julkisivun
    /// suorittajakarttaan (suoritus).
    ///
    /// # Errors
    /// Palauttaa manifestin validoinnin tai duplikaattirekisteröinnin virheen
    /// ([`Pipeline::register_skill`]).
    pub fn register_skill<S>(&mut self, skill: S) -> Result<()>
    where
        S: Skill + 'static,
    {
        self.pipeline.register_skill(&skill)?;
        let id = skill.manifest().id;
        self.executors.insert(id, Arc::new(skill));
        Ok(())
    }

    /// Luettelee rekisteröidyt taidot tiivistettyinä (tunniste, nimi, versio,
    /// riskiluokka, hyväksyntävaatimus). Järjestys on nimen mukaan vakautettu.
    ///
    /// Tuloste ei koskaan sisällä salaisuuksia — manifesti on jo validoitu
    /// salaisuudettomaksi rekisteröintihetkellä.
    #[must_use]
    pub fn list_skills(&self) -> Vec<SkillSummary> {
        let mut out: Vec<SkillSummary> = self
            .pipeline
            .registry()
            .list()
            .into_iter()
            .map(|m| SkillSummary {
                id: m.id,
                name: m.name.clone(),
                version: m.version.clone(),
                risk: m.risk,
                requires_approval: crate::policy::required_approval(m.risk, m.approval_policy)
                    .requires_approval(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Palauttaa **raa'at** MCP-työkalukuvaukset jokaista rekisteröityä taitoa
    /// kohti — juuri se data jonka agentti tarvitsee rakentaakseen LLM:lle
    /// tarjottavat työkalumääritelmät.
    ///
    /// ## Kerrosvastuu (tarkoituksellinen)
    /// Tämä julkisivu **ei** tunne `familyclaw-agent`-kerrosta eikä rakenna
    /// lopullista LLM-`ToolDefinition`-arvoa. Se paljastaa vain
    /// [`McpToolDescriptor`]-kuvaukset (nimi, kuvaus, syöteskeema, vaadittu
    /// oikeus, luotettavuus); agentti kokoaa niistä oman muotonsa ja reitittää
    /// työkalukutsun takaisin taitoon [`ActionRuntime::map_name_to_skill`]:lla.
    /// Näin riippuvuus kulkee vain suuntaan agentti → actions, ei takaisin.
    ///
    /// ## Johdanto manifestista
    /// Jokainen kuvaus johdetaan taidon validoidusta
    /// [`crate::manifest::SkillManifest`]-manifestista:
    /// - `name` ← manifestin nimi (sama jolla [`ActionRuntime::map_name_to_skill`]
    ///   reitittää kutsun takaisin),
    /// - `description` ← manifestin kuvaus,
    /// - `input_schema` ← manifestin koneluettava syöteskeema
    ///   ([`crate::manifest::SkillManifest::input_schema`]); juuri on aina
    ///   JSON-objekti (validointi takaa tämän), joten se kelpaa LLM:n työkalun
    ///   `parameters`-kentäksi sellaisenaan,
    /// - `required_permission` ← manifestin oikeuksista **tiukin** yksittäinen
    ///   oikeus (sivuvaikutuksiltaan vakavin); jos taito ei vaadi oikeuksia,
    ///   oletus on [`SkillPermission::ReadFiles`] (kaikkein vähiten oikeuttava),
    /// - `trusted` ← aina `false`: taidosta johdetun työkalun tuloste
    ///   käsitellään oletuksena epäluotettavana, kuten muuallakin cratessa.
    ///
    /// Järjestys on nimen mukaan vakautettu (sama kuin
    /// [`ActionRuntime::list_skills`]), tasapeli ratkaistaan tunnisteella, jotta
    /// tuloste on toistettava.
    ///
    /// Tuloste ei koskaan sisällä salaisuuksia — manifesti on validoitu
    /// salaisuudettomaksi jo rekisteröintihetkellä.
    #[must_use]
    pub fn tool_definitions(&self) -> Vec<McpToolDescriptor> {
        let mut out: Vec<(SkillId, McpToolDescriptor)> = self
            .pipeline
            .registry()
            .list()
            .into_iter()
            .map(|m| {
                let descriptor = McpToolDescriptor::new(
                    m.name.clone(),
                    m.description.clone(),
                    m.input_schema.clone(),
                    strictest_permission(&m.permissions),
                );
                (m.id, descriptor)
            })
            .collect();
        out.sort_by(|a, b| {
            a.1.name
                .cmp(&b.1.name)
                .then_with(|| a.0.cmp(&b.0))
        });
        out.into_iter().map(|(_, d)| d).collect()
    }

    /// Reitittää työkalun nimen takaisin sitä vastaavaan taidon tunnisteeseen.
    ///
    /// Agentti kutsuu tätä kun LLM valitsee työkalun nimellä (sama nimi jonka
    /// [`ActionRuntime::tool_definitions`] julkaisi): nimestä saadaan
    /// [`SkillId`], jolla tehtävän voi lähettää eteenpäin
    /// [`ActionRuntime::submit_task`]:lle. Palauttaa `None`, jos millään
    /// rekisteröidyllä taidolla ei ole tätä nimeä.
    ///
    /// Haku on tarkka merkkijonovertailu manifestin nimeen. Jos kaksi taitoa
    /// jakaisi saman nimen, palautetaan vakautetusti pienin tunniste, jotta
    /// reititys on deterministinen (käytännössä nimet ovat uniikkeja).
    #[must_use]
    pub fn map_name_to_skill(&self, name: &str) -> Option<SkillId> {
        self.pipeline
            .registry()
            .list()
            .into_iter()
            .filter(|m| m.name == name)
            .map(|m| m.id)
            .min()
    }

    /// Lähettää tehtävän annetulle taidolle ja ajaa putken.
    ///
    /// Jos taidon riskiluokka sallii auto-runin, putki suorittaa toiminnon
    /// loppuun ja todiste tallennetaan. Jos käytäntö vaatii ihmisen
    /// hyväksynnän, tehtävä jää tilaan [`TaskStatus::NeedsApproval`] ja
    /// julkisivu **myöntää** payload-sidotun hyväksynnän jonka tunniste
    /// palautetaan ([`SubmitOutcome::pending_approval`]); suorituksen voi
    /// jatkaa [`ActionRuntime::approve`]-kutsulla.
    ///
    /// # Errors
    /// - [`ActionError::UnknownSkill`] jos taitoa ei ole rekisteröity.
    /// - Putken jono-, suoritus- tai todistevirheet.
    pub async fn submit_task(
        &mut self,
        skill_id: SkillId,
        payload: Value,
        now: Timestamp,
    ) -> Result<SubmitOutcome> {
        let executor = self
            .executors
            .get(&skill_id)
            .ok_or_else(|| ActionError::UnknownSkill(skill_id.to_string()))?
            .clone();

        let task = ActionTask::new(skill_id, payload.clone(), now);
        let task_id = task.id;

        let outcome = self.pipeline.run(executor.as_ref(), task, now).await?;

        if let Some(proof) = outcome.proof {
            self.proofs.insert(task_id, proof);
        }

        let pending_approval = if outcome.awaiting_approval {
            let approval = self.pipeline.grant_approval(
                outcome.action_id,
                &payload,
                now,
                Duration::minutes(DEFAULT_APPROVAL_TTL_MINUTES),
            )?;
            let approval_id = approval.id;
            self.pending
                .insert(approval_id, PendingEntry { task_id, approval });
            Some(approval_id)
        } else {
            None
        };

        Ok(SubmitOutcome {
            task_id,
            status: outcome.status,
            pending_approval,
        })
    }

    /// Kuluttaa (merkitsee käytetyksi) odottavan hyväksynnän ja ajaa pysähtyneen
    /// tehtävän suorituksen loppuun.
    ///
    /// Hyväksyntä kulutetaan tehtävän tallennettua payloadia vasten
    /// (payload-sidonta + kertakäyttö), joten muutettu payload ei voi käyttää
    /// hyväksyntää. Onnistuessa syntyvä todiste tallennetaan haettavaksi.
    ///
    /// # Errors
    /// - [`ActionError::ApprovalMissing`] jos hyväksyntää ei ole odottamassa.
    /// - [`ActionError::UnknownSkill`] jos tehtävän taitoa ei (enää) löydy.
    /// - Hyväksynnän kulutuksen tai putken virheet
    ///   ([`Pipeline::run_after_approval`]).
    pub async fn approve(
        &mut self,
        approval_id: ApprovalId,
        now: Timestamp,
    ) -> Result<SubmitOutcome> {
        let entry = self
            .pending
            .get(&approval_id)
            .cloned()
            .ok_or_else(|| ActionError::ApprovalMissing(approval_id.to_string()))?;

        let task = self
            .pipeline
            .queue()
            .get(entry.task_id)
            .await
            .ok_or_else(|| ActionError::NotFound(format!("tehtävää {} ei löydy", entry.task_id)))?;
        let executor = self
            .executors
            .get(&task.skill_id)
            .ok_or_else(|| ActionError::UnknownSkill(task.skill_id.to_string()))?
            .clone();

        let outcome = self
            .pipeline
            .run_after_approval(executor.as_ref(), entry.task_id, &entry.approval, now)
            .await?;

        // Hyväksyntä on nyt kulutettu — poista se odottavista.
        self.pending.remove(&approval_id);

        if let Some(proof) = outcome.proof {
            self.proofs.insert(entry.task_id, proof);
        }

        Ok(SubmitOutcome {
            task_id: entry.task_id,
            status: outcome.status,
            pending_approval: None,
        })
    }

    /// Palauttaa tehtävän tilan tunnisteella; `None` jos tehtävää ei ole jonossa.
    pub async fn status(&self, task_id: ActionTaskId) -> Option<TaskStatus> {
        self.pipeline.queue().get(task_id).await.map(|t| t.status)
    }

    /// Palauttaa tehtävälle syntyneen **redaktoidun** todistepaketin; `None` jos
    /// todistetta ei (vielä) ole (esim. tehtävä odottaa yhä hyväksyntää).
    ///
    /// Todiste on jo redaktoitu putkessa — se ei koskaan sisällä raakaa
    /// payloadia eikä salaisuuksia.
    #[must_use]
    pub fn proof(&self, task_id: ActionTaskId) -> Option<&ProofBundle> {
        self.proofs.get(&task_id)
    }

    /// Luettelee odottavat hyväksynnät (salaisuudettomat tiivistelmät).
    ///
    /// Järjestys vakautetaan hyväksynnän tunnisteen mukaan toistettavuuden
    /// vuoksi.
    #[must_use]
    pub fn pending_approvals(&self) -> Vec<PendingApproval> {
        let mut out: Vec<PendingApproval> = self
            .pending
            .values()
            .map(|e| PendingApproval {
                approval_id: e.approval.id,
                task_id: e.task_id,
            })
            .collect();
        out.sort_by_key(|a| a.approval_id);
        out
    }
}

/// Valitsee joukosta oikeuksia **tiukimman** yksittäisen oikeuden, jolla
/// taidosta johdettu MCP-työkalu portitetaan
/// ([`McpToolDescriptor::required_permission`] on yksiarvoinen).
///
/// Taidon manifesti voi ilmoittaa useita oikeuksia, mutta työkalukuvaus
/// gettaa vain yhdellä. Valitaan kaikkein eniten oikeuttava (sivuvaikutuksiltaan
/// vakavin), jotta agentti vaatii kutsujalta vahvimman tarvittavan capabilityn —
/// fail-safe: koskaan ei aliarvioida vaadittua oikeutta. Vakavuusjärjestys
/// kasvavasti:
///
/// ```text
/// ReadFiles < NetworkRead < WriteLocalFiles < SendMessage
///           < ExecuteCode < WriteExternal < SpendMoney
/// ```
///
/// Jos lista on tyhjä (taito ei vaadi oikeuksia), palautetaan kaikkein vähiten
/// oikeuttava [`SkillPermission::ReadFiles`].
fn strictest_permission(permissions: &[SkillPermission]) -> SkillPermission {
    permissions
        .iter()
        .copied()
        .max_by_key(|p| permission_severity(*p))
        .unwrap_or(SkillPermission::ReadFiles)
}

/// Yksittäisen oikeuden vakavuusaste (suurempi = enemmän oikeuttava /
/// sivuvaikutuksiltaan vakavampi). Käytetään [`strictest_permission`]:ssa
/// valitsemaan tiukin oikeus deterministisesti.
const fn permission_severity(permission: SkillPermission) -> u8 {
    match permission {
        SkillPermission::ReadFiles => 0,
        SkillPermission::NetworkRead => 1,
        SkillPermission::WriteLocalFiles => 2,
        SkillPermission::SendMessage => 3,
        SkillPermission::ExecuteCode => 4,
        SkillPermission::WriteExternal => 5,
        SkillPermission::SpendMoney => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{EmailTriageMock, FilePatchMock, GithubIssueDraftMock};
    use familyclaw_core::time::from_unix_secs;
    use serde_json::json;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    #[test]
    fn default_skills_are_listed_without_secrets() {
        let runtime = ActionRuntime::with_default_skills().expect("default skills");
        let skills = runtime.list_skills();
        assert_eq!(skills.len(), 5, "all five default skills registered");

        // Nimet aakkostettu → deterministinen järjestys.
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);

        // Tuloste ei sisällä salaisuuksia (vain julkiset kentät).
        let rendered = serde_json::to_string(&skills).expect("serialize summaries");
        assert!(!rendered.contains("sk-"));
        assert!(!rendered.contains("Bearer "));
    }

    #[test]
    fn tool_definitions_mirror_skills_sorted_without_secrets() {
        let runtime = ActionRuntime::with_default_skills().expect("default skills");
        let tools = runtime.tool_definitions();
        assert_eq!(tools.len(), 5, "one descriptor per registered skill");

        // Sama vakautettu nimijärjestys kuin list_skills.
        let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        let skill_names: Vec<String> = runtime
            .list_skills()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(tool_names, skill_names);

        // Jokaisen kuvauksen syöteskeema on manifestin skeema (juuri objekti) ja
        // lähde on oletuksena epäluotettu.
        for tool in &tools {
            let id = runtime
                .map_name_to_skill(&tool.name)
                .expect("tool name maps to a skill");
            let manifest = runtime
                .pipeline
                .registry()
                .get(&id)
                .expect("mapped skill in registry");
            assert_eq!(tool.input_schema, manifest.input_schema);
            assert!(
                tool.input_schema.is_object(),
                "schema root must be a JSON object for LLM parameters"
            );
            assert!(!tool.trusted, "skill-derived tools default to untrusted");
            assert!(!tool.description.is_empty());
        }

        // Ei salaisuuksia tulosteessa.
        let rendered = serde_json::to_string(&tools).expect("serialize descriptors");
        assert!(!rendered.contains("sk-"));
        assert!(!rendered.contains("Bearer "));
    }

    #[test]
    fn map_name_to_skill_roundtrips_with_tool_definitions() {
        let runtime = ActionRuntime::with_default_skills().expect("default skills");

        // Jokainen julkaistu työkalunimi reitittyy takaisin taidon tunnisteeseen.
        for tool in runtime.tool_definitions() {
            let id = runtime
                .map_name_to_skill(&tool.name)
                .expect("known tool name maps to a skill");
            // Tunniste vastaa rekisterin manifestin tunnistetta.
            let manifest = runtime
                .pipeline
                .registry()
                .get(&id)
                .expect("mapped id exists in registry");
            assert_eq!(manifest.name, tool.name);
        }
    }

    #[test]
    fn map_name_to_skill_unknown_is_none() {
        let runtime = ActionRuntime::with_default_skills().expect("default skills");
        assert!(runtime.map_name_to_skill("does_not_exist").is_none());
    }

    #[test]
    fn tool_definition_required_permission_is_strictest() {
        // GitHub issue draft -taito kirjoittaa ulkoiseen järjestelmään →
        // tiukimman oikeuden on oltava write_external (ei esim. network_read).
        let runtime = ActionRuntime::with_default_skills().expect("default skills");
        let id = GithubIssueDraftMock::skill_id();
        let manifest = runtime
            .pipeline
            .registry()
            .get(&id)
            .expect("github skill registered");
        let expected = super::strictest_permission(&manifest.permissions);

        let tool = runtime
            .tool_definitions()
            .into_iter()
            .find(|t| t.name == manifest.name)
            .expect("github tool published");
        assert_eq!(tool.required_permission, expected);
    }

    #[test]
    fn strictest_permission_picks_most_privileged() {
        // Tyhjä lista → vähiten oikeuttava oletus.
        assert_eq!(
            super::strictest_permission(&[]),
            SkillPermission::ReadFiles
        );
        // Sekalainen joukko → tiukin (spend_money).
        assert_eq!(
            super::strictest_permission(&[
                SkillPermission::ReadFiles,
                SkillPermission::SpendMoney,
                SkillPermission::NetworkRead,
            ]),
            SkillPermission::SpendMoney
        );
        // Write_external voittaa send_messagen.
        assert_eq!(
            super::strictest_permission(&[
                SkillPermission::SendMessage,
                SkillPermission::WriteExternal,
            ]),
            SkillPermission::WriteExternal
        );
    }

    #[tokio::test]
    async fn read_only_task_auto_runs_and_produces_proof() {
        let mut runtime = ActionRuntime::with_default_skills().expect("default skills");
        let now = at(1_700_000_000);

        // Email triage on read-only → auto-run, ei hyväksyntää.
        let payload = json!({
            "emails": [
                { "from": "user@example.com", "subject": "Invoice question", "body": "When is it due?" }
            ]
        });
        let outcome = runtime
            .submit_task(EmailTriageMock::skill_id(), payload, now)
            .await
            .expect("submit");

        assert_eq!(outcome.status, TaskStatus::Done);
        assert!(!outcome.awaiting_approval());
        assert!(outcome.pending_approval.is_none());

        // Status on Done, todiste haettavissa.
        assert_eq!(
            runtime.status(outcome.task_id).await,
            Some(TaskStatus::Done)
        );
        let proof = runtime.proof(outcome.task_id).expect("proof present");
        assert_eq!(proof.task_id, outcome.task_id);
        assert!(proof.verification.verified);
    }

    #[tokio::test]
    async fn write_external_task_waits_for_approval_then_completes() {
        let mut runtime = ActionRuntime::with_default_skills().expect("default skills");
        let now = at(1_700_000_000);

        // GitHub issue draft on write-external → vaatii hyväksynnän.
        let payload = json!({ "bug_report": "Login button does nothing" });
        let submitted = runtime
            .submit_task(GithubIssueDraftMock::skill_id(), payload, now)
            .await
            .expect("submit");

        assert_eq!(submitted.status, TaskStatus::NeedsApproval);
        assert!(submitted.awaiting_approval());
        let approval_id = submitted.pending_approval.expect("approval granted");

        // Odottava hyväksyntä näkyy luettelossa.
        let pending = runtime.pending_approvals();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].approval_id, approval_id);
        assert_eq!(pending[0].task_id, submitted.task_id);

        // Ennen hyväksyntää todistetta ei ole.
        assert!(runtime.proof(submitted.task_id).is_none());

        // Hyväksy → suoritus loppuun, todiste syntyy.
        let approved = runtime.approve(approval_id, now).await.expect("approve");
        assert_eq!(approved.task_id, submitted.task_id);
        assert_eq!(approved.status, TaskStatus::Done);

        // Hyväksyntä kulutettu → ei enää odottavissa.
        assert!(runtime.pending_approvals().is_empty());
        // Todiste nyt haettavissa.
        assert!(runtime.proof(submitted.task_id).is_some());
        assert_eq!(
            runtime.status(submitted.task_id).await,
            Some(TaskStatus::Done)
        );
    }

    #[tokio::test]
    async fn submit_unknown_skill_fails() {
        let mut runtime = ActionRuntime::new();
        let err = runtime
            .submit_task(SkillId::new(), json!({}), at(1))
            .await
            .expect_err("unknown skill must fail");
        assert!(matches!(err, ActionError::UnknownSkill(_)));
    }

    #[tokio::test]
    async fn approve_unknown_approval_fails_closed() {
        let mut runtime = ActionRuntime::with_default_skills().expect("default skills");
        let err = runtime
            .approve(ApprovalId::new(), at(1))
            .await
            .expect_err("unknown approval must fail closed");
        assert!(matches!(err, ActionError::ApprovalMissing(_)));
    }

    #[tokio::test]
    async fn approval_cannot_be_reused() {
        let mut runtime = ActionRuntime::with_default_skills().expect("default skills");
        let now = at(1_700_000_000);

        let submitted = runtime
            .submit_task(
                FilePatchMock::skill_id(),
                json!({ "file_content": "line one\n", "requested_edit": "add a line" }),
                now,
            )
            .await
            .expect("submit");
        let approval_id = submitted
            .pending_approval
            .expect("file patch requires approval");

        runtime
            .approve(approval_id, now)
            .await
            .expect("first approve");

        // Toinen kulutus epäonnistuu: hyväksyntä poistettiin odottavista.
        let err = runtime
            .approve(approval_id, now)
            .await
            .expect_err("second approve must fail closed");
        assert!(matches!(err, ActionError::ApprovalMissing(_)));
    }

    #[tokio::test]
    async fn status_and_proof_for_missing_task_are_none() {
        let runtime = ActionRuntime::new();
        let missing = ActionTaskId::new();
        assert!(runtime.status(missing).await.is_none());
        assert!(runtime.proof(missing).is_none());
    }

    /// Testitaito joka kaiuttaa payloadin `secret`-kentän arvon suoraan
    /// tulosteeseen standalone-arvona. Käytetään todistamaan, että julkisivun
    /// kautta syntyvä todistepaketti redaktoidaan (KERROS A — vain testikäyttö).
    #[derive(Debug, Clone, Default)]
    struct EchoSecretSkill;

    /// Testitaidon kiinteä tunniste.
    const ECHO_SKILL_UUID: uuid::Uuid = uuid::uuid!("99999999-9999-4999-8999-999999999999");

    #[async_trait::async_trait]
    impl ActionExecutor for EchoSecretSkill {
        async fn execute(
            &self,
            request: crate::executor::ActionRequest,
        ) -> Result<crate::executor::ActionResult> {
            // Kaiuta payloadin "secret"-kenttä tulosteeseen standalone-arvona.
            let echoed = request
                .payload
                .get("secret")
                .cloned()
                .unwrap_or(Value::Null);
            Ok(crate::executor::ActionResult::success(
                "echoed input value",
                json!({ "echoed": echoed }),
                request.now,
            ))
        }
    }

    impl Skill for EchoSecretSkill {
        fn manifest(&self) -> crate::manifest::SkillManifest {
            crate::manifest::SkillManifest {
                id: SkillId::from_uuid(ECHO_SKILL_UUID),
                name: "echo_secret_test".to_string(),
                version: "1.0.0".to_string(),
                description: "Kaiuttaa syötteen tulosteeseen (vain luku, testikäyttö).".to_string(),
                permissions: vec![crate::policy::SkillPermission::NetworkRead],
                risk: ActionRisk::ReadOnly,
                approval_policy: crate::policy::ApprovalPolicy::AutoIfReadOnly,
                input_hint: None,
                output_hint: None,
                input_schema: crate::manifest::default_input_schema(),
            }
        }
    }

    #[tokio::test]
    async fn proof_is_redacted_for_secret_looking_payload() {
        let mut runtime = ActionRuntime::new();
        runtime
            .register_skill(EchoSecretSkill)
            .expect("register echo skill");
        let now = at(1_700_000_000);

        // Salaisuus rakennetaan ajonaikana (ei literaalia lähteessä, Layer B).
        let fake = format!("sk-{}", "live".repeat(4));
        // Taito kaiuttaa salaisuuden standalone-arvona → ilman redaktointia se
        // kulkisi todisteen redacted_output-kenttään.
        let payload = json!({ "secret": fake.clone() });
        let outcome = runtime
            .submit_task(SkillId::from_uuid(ECHO_SKILL_UUID), payload, now)
            .await
            .expect("submit");
        assert_eq!(outcome.status, TaskStatus::Done);

        let proof = runtime.proof(outcome.task_id).expect("proof present");
        // Tuloste redaktoitiin: raakaa salaisuutta ei ole missään todisteessa.
        assert!(
            proof.redaction.any_redacted(),
            "secret-looking output value must be redacted"
        );
        let whole = serde_json::to_string(proof).expect("serialize proof");
        assert!(
            !whole.contains(&fake),
            "proof must never contain raw secret"
        );
    }
}
