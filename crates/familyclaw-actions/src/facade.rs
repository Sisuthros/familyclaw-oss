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

use crate::dispatch_outbox::{
    DispatchLookup, DispatchOutboxStore, DispatchedOutcome, InMemoryDispatchOutbox,
};
use crate::error::{ActionError, Result};
use crate::executor::ActionExecutor;
use crate::ids::{ActionTaskId, ApprovalId, SkillId};
use crate::mcp::McpToolDescriptor;
use crate::pending_store::{
    DangerousToolRateLimiter, InMemoryPendingStore, PendingApprovalStore, PendingRecord,
};
use crate::policy::{ActionRisk, SkillPermission};
use crate::proof::ProofBundle;
use crate::skills::{
    DiscordThreadSummaryMock, EmailTriageMock, FilePatchMock, FsReadAllowlisted,
    GithubIssueDraftMock, Pipeline, Skill,
};
use crate::task::{ActionTask, DurableTaskQueue, TaskQueue, TaskStatus};

/// Moduulin valmiusaste — säilytetään, jotta [`crate::all_modules_scaffolded`]
/// kääntyy edelleen muiden moduulien rinnalla.
pub(crate) const SCAFFOLDED: bool = true;

/// Hyväksyntäpyynnön oletus-TTL kun operaattori myöntää hyväksynnän
/// (`submit-task` jättää tehtävän odottamaan; hyväksyntä on voimassa tämän ajan).
const DEFAULT_APPROVAL_TTL_MINUTES: i64 = 60;

/// Vaarallisten (hyväksyntää vaativien) työkalukutsujen per-olento-rate-limitin
/// **liukuvan ikkunan** oletuspituus sekunteina (1 tunti).
///
/// Yhdessä [`DEFAULT_DANGEROUS_TOOL_LIMIT`]:n kanssa tämä muodostaa
/// tarkoituksella **sallivan oletuksen**: ihmissilmukassa yksi olento ei
/// käytännössä lähetä satoja hyväksyntää vaativia toimintoja tunnissa, joten
/// oletus ei häiritse normaalia käyttöä mutta katkaisee selvän tulvituksen.
const DEFAULT_DANGEROUS_TOOL_WINDOW_SECS: i64 = 3_600;

/// Vaarallisten työkalukutsujen per-olento-rate-limitin **oletuskatto** yhdessä
/// ikkunassa ([`DEFAULT_DANGEROUS_TOOL_WINDOW_SECS`]).
///
/// Saliva oletus (256 hyväksyntää vaativaa toimintoa per olento per tunti):
/// reilusti normaalin ihmissilmukan yläpuolella mutta rajaa silti yhden olennon
/// kyvyn tulvittaa hyväksyntöjen jonoa. Operaattori voi tiukentaa tätä
/// ([`ActionRuntime::with_rate_limiter`]).
const DEFAULT_DANGEROUS_TOOL_LIMIT: usize = 256;

/// Geneerinen oletus-olentotunniste rate-limit-laskennassa, kun kutsuja ei anna
/// nimenomaista olentoa ([`ActionRuntime::submit_task`]).
///
/// Tarkoituksella neutraali (**ei** perheenjäsenen nimeä): kaikki saman
/// ajoympäristön kautta nimettömästi lähetetyt vaaralliset toiminnot jakavat
/// tämän kiintiön. Anna oikea olento [`ActionRuntime::submit_task_as`]:lla, kun
/// useampi olento jakaa saman ajoympäristön ja kullekin halutaan oma kiintiö.
const DEFAULT_BEING_ID: &str = "operator";

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
///
/// ## Odottavien hyväksyntöjen tallennus
/// Odottavat hyväksynnät eivät elä enää pelkässä `HashMap`:ssa vaan
/// [`PendingApprovalStore`]-traitin takana (sisäinen `pending`-kenttä).
/// Oletus on [`InMemoryPendingStore`] (sama käyttäytyminen kuin ennen), mutta
/// operaattori voi vaihtaa tilalle kaatumiskestävän
/// [`crate::pending_store::JournalPendingStore`]:n
/// ([`ActionRuntime::with_pending_store`]), jolloin `submit-task`:n ja
/// `approve`:n välinen kaatuminen **ei** enää menetä odottavaa hyväksyntää.
pub struct ActionRuntime {
    /// Koko toimintopinon putki (rekisteri + jono + ledger + audit).
    pipeline: Pipeline,
    /// Taidon tunniste → suorittaja, suoritusta varten.
    executors: HashMap<SkillId, Arc<dyn ActionExecutor>>,
    /// Tehtävän tunniste → syntynyt redaktoitu todistepaketti.
    proofs: HashMap<ActionTaskId, ProofBundle>,
    /// Odottavien hyväksyntöjen tallennuspinta (oletuksena muistinvarainen,
    /// vaihdettavissa kaatumiskestäväksi).
    pending: Box<dyn PendingApprovalStore>,
    /// **Kaatumiskestävä tehtäväjono** (valinnainen). Kun asetettu
    /// ([`ActionRuntime::with_durable_stores`]), jokainen `submit-task`:n ja
    /// `approve`:n tuottama tehtävän tilannekuva mirroroidaan tähän JSONL-
    /// lokiin, ja uudelleenkäynnistyksessä putken jono rekonstruoidaan siitä —
    /// niin että hyväksyntää odottava tehtävä on yhä `approve`-kelpoinen vaikka
    /// prosessi olisi kaatunut `submit-task`:n ja `approve`:n välissä.
    /// `None` → in-memory-jono (ei selviä kaatumisesta), kuten oletuksena.
    durable_queue: Option<DurableTaskQueue>,
    /// **Per-olento-rate-limit vaarallisille (hyväksyntää vaativille)
    /// työkalukutsuille.** Tarkistetaan `submit-task`:ssa **ennen** hyväksynnän
    /// myöntämistä: jos olento on jo käyttänyt kiintiönsä liukuvassa ikkunassa,
    /// `submit-task` hylkää fail-closed ([`ActionError::PolicyDenied`]) myöntämättä
    /// hyväksyntää eikä jätä tehtävää odottamaan.
    ///
    /// Kapasiteettikatto ([`crate::pending_store::PendingCapacity`]) on **globaali**
    /// (koko jono); tämä rajoitin lisää siihen **per-olento**-katon, jottei yksi
    /// olento voi yksin täyttää jonoa. Auto-run-tehtäviä (luku / paikallinen
    /// kirjoitus) ei rate-limititä — vain ne jotka jäisivät odottamaan ihmisen
    /// hyväksyntää.
    ///
    /// Oletus on **salliva** ([`DEFAULT_DANGEROUS_TOOL_WINDOW_SECS`] /
    /// [`DEFAULT_DANGEROUS_TOOL_LIMIT`]); operaattori voi tiukentaa sen
    /// [`ActionRuntime::with_rate_limiter`]:lla.
    rate_limiter: DangerousToolRateLimiter,
    /// **Oletus-olentotunniste** rate-limit-laskennassa kun
    /// [`ActionRuntime::submit_task`]:ia kutsutaan ilman nimenomaista olentoa.
    ///
    /// Oletus on geneerinen [`DEFAULT_BEING_ID`] (ei perheenjäsenen nimeä). Käytä
    /// [`ActionRuntime::submit_task_as`]:ia antaaksesi olennon per kutsu, tai
    /// [`ActionRuntime::with_being_id`]:tä asettaaksesi tämän ajoympäristön
    /// oletusolennon.
    being_id: String,
    /// **Lähetyksen idempotenssi-outbox** (at-most-once-rajan kivijalka:
    /// kaksoislaukaisun esto kaatumisen yli, EI universaali exactly-once
    /// completion).
    ///
    /// [`ActionRuntime::submit_task_idempotent`] kytkee jokaiseen lähetykseen
    /// kutsujan johtaman vakaan avaimen ja kirjaa lähetyksen kaksivaiheisesti
    /// (intent ennen sivuvaikutusta, committed sen jälkeen) tähän outboxiin. Kun
    /// sama avain nähdään uudelleen (replay/restart), jo sitoutunut lähetys
    /// palautuu **arvo-identtisenä ajamatta sivuvaikutusta uudelleen** — riippumatta
    /// siitä mihin agenttikerroksen oma journal-append-ikkuna kaatumisessa osui.
    ///
    /// Oletus on [`InMemoryDispatchOutbox`] (ei selviä kaatumisesta, sama
    /// käyttäytyminen kuin ennen outboxia); kaatumiskestävyyteen anna
    /// [`crate::dispatch_outbox::JournalDispatchOutbox`]
    /// ([`ActionRuntime::with_dispatch_outbox`]).
    dispatch_outbox: Box<dyn DispatchOutboxStore>,
}

impl Default for ActionRuntime {
    /// Oletus: tyhjä ajoympäristö jonka odottavat hyväksynnät elävät
    /// muistinvaraisessa pinnassa ([`InMemoryPendingStore`]).
    fn default() -> Self {
        Self {
            pipeline: Pipeline::default(),
            executors: HashMap::new(),
            proofs: HashMap::new(),
            pending: Box::new(InMemoryPendingStore::new()),
            durable_queue: None,
            rate_limiter: DangerousToolRateLimiter::new(
                DEFAULT_DANGEROUS_TOOL_WINDOW_SECS,
                DEFAULT_DANGEROUS_TOOL_LIMIT,
            ),
            being_id: DEFAULT_BEING_ID.to_string(),
            dispatch_outbox: Box::new(InMemoryDispatchOutbox::new()),
        }
    }
}

impl std::fmt::Debug for ActionRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionRuntime")
            .field("pipeline", &self.pipeline)
            .field("executor_count", &self.executors.len())
            .field("proofs", &self.proofs.len())
            .field("pending_count", &self.pending.len().unwrap_or(0))
            .field("durable_queue", &self.durable_queue)
            .field("rate_limiter", &self.rate_limiter)
            .field("being_id", &self.being_id)
            .field("dispatch_outbox", &self.dispatch_outbox)
            .finish()
    }
}

impl ActionRuntime {
    /// Luo uuden tyhjän ajoympäristön ilman rekisteröityjä taitoja.
    ///
    /// Odottavat hyväksynnät elävät oletuksena muistinvaraisessa pinnassa
    /// ([`InMemoryPendingStore`]) — käytä [`ActionRuntime::with_pending_store`]:a
    /// kaatumiskestävään tallennukseen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Luo tyhjän ajoympäristön annetulla **odottavien hyväksyntöjen
    /// tallennuspinnalla**.
    ///
    /// Tämä on koukku kaatumiskestävyyteen: anna
    /// [`crate::pending_store::JournalPendingStore`], niin
    /// `submit-task`:n myöntämä mutta vielä hyväksymätön toiminto **säilyy
    /// prosessin kaatumisen yli** ja on yhä [`ActionRuntime::approve`]-kelpoinen
    /// uudelleenkäynnistyksen jälkeen. Oletustallennus ([`ActionRuntime::new`])
    /// on muistinvarainen eikä selviä kaatumisesta.
    #[must_use]
    pub fn with_pending_store(pending: Box<dyn PendingApprovalStore>) -> Self {
        Self {
            pending,
            ..Self::default()
        }
    }

    /// Vaihtaa ajoympäristön **vaarallisten työkalukutsujen rate-limitin**
    /// (per-olento, liukuva ikkuna) annettuun rajoittimeen ja palauttaa itsensä
    /// (builder-tyyli).
    ///
    /// Tämä on operaattorin koukku tiukentaa (tai löysentää) sallivaa oletusta
    /// (`DEFAULT_DANGEROUS_TOOL_WINDOW_SECS` / `DEFAULT_DANGEROUS_TOOL_LIMIT`).
    /// Rajoitin tarkistetaan `submit-task`:ssa **ennen** hyväksynnän myöntämistä
    /// vain niille tehtäthe operator jotka jäisivät odottamaan ihmisen hyväksyntää —
    /// auto-run-tehtäviä (luku / paikallinen kirjoitus) ei rate-limititä.
    ///
    /// ```
    /// # use familyclaw_actions::ActionRuntime;
    /// # use familyclaw_actions::pending_store::DangerousToolRateLimiter;
    /// // Korkeintaan 3 hyväksyntää vaativaa toimintoa per olento per 60 s.
    /// let runtime = ActionRuntime::new()
    ///     .with_rate_limiter(DangerousToolRateLimiter::new(60, 3));
    /// let _ = runtime;
    /// ```
    #[must_use]
    pub fn with_rate_limiter(mut self, rate_limiter: DangerousToolRateLimiter) -> Self {
        self.rate_limiter = rate_limiter;
        self
    }

    /// Asettaa ajoympäristön **oletus-olentotunnisteen** rate-limit-laskentaa
    /// varten ja palauttaa itsensä (builder-tyyli).
    ///
    /// Tätä olentoa käytetään kun [`ActionRuntime::submit_task`]:ia kutsutaan
    /// ilman nimenomaista olentoa. Käytä geneeristä, **ei-henkilökohtaista**
    /// tunnistetta (esim. `"agent-a"` / `"operator"`). Per-kutsu-olennon voi antaa
    /// suoraan [`ActionRuntime::submit_task_as`]:lla ilman tätä asetusta.
    #[must_use]
    pub fn with_being_id(mut self, being_id: impl Into<String>) -> Self {
        self.being_id = being_id.into();
        self
    }

    /// Luo ajoympäristön **täysin kaatumiskestävällä** suspend/resume-tilalla:
    /// kaatumiskestävä odottavien hyväksyntöjen pinta **ja** kaatumiskestävä
    /// tehtäväjono, molemmat rekonstruoituina annetuista durable-tiedostoista.
    ///
    /// Tämä on suspend/resume-sillan (roadmap §6) actions-puolen
    /// kaatumiskestävyys: pelkkä [`ActionRuntime::with_pending_store`] säilyttää
    /// odottavan **hyväksynnän**, mutta `approve` tarvitsee myös tehtävän
    /// (payload + tila) putken jonossa ja itse hyväksynnän ledgerissä. Kaikki
    /// kolme menetetään prosessin kaatuessa, ellei niitä persistoida. Tämä
    /// konstruktori:
    ///
    /// 1. rakentaa kaatumiskestävän **pending-pinnan** annetusta polusta
    ///    ([`crate::pending_store::JournalPendingStore`]),
    /// 2. rekonstruoi **tehtäväjonon** durable-jonosta
    ///    ([`DurableTaskQueue::reload`] → [`TaskQueue::from_map`]),
    /// 3. **palauttaa ledgeriin** jokaisen odottavan hyväksynnän durable-
    ///    pinnalta ([`crate::pending_store::PendingRecord::approval`]), jotta
    ///    `approve` voi kuluttaa sen samalla payload-sidonnalla,
    /// 4. mirroroi jatkossa jokaisen tehtävän tilannekuvan durable-jonoon, jotta
    ///    uudelleenkäynnistys löytää sen.
    ///
    /// Taidot rekisteröidään tämän jälkeen normaalisti
    /// ([`ActionRuntime::register_skill`]); ne ovat puhdasta koodia eivätkä
    /// tarvitse persistointia.
    ///
    /// # Errors
    /// - [`ActionError::Proof`] jos pending- tai task-journalin avaus/luku
    ///   epäonnistuu.
    pub async fn with_durable_stores(
        pending_path: impl AsRef<std::path::Path>,
        task_queue_path: impl Into<std::path::PathBuf>,
    ) -> Result<Self> {
        let pending: Box<dyn PendingApprovalStore> =
            Box::new(crate::pending_store::JournalPendingStore::open(pending_path)?);
        let durable_queue = DurableTaskQueue::new(task_queue_path);

        // Rekonstruoi tehtäväjono levyltä → putki palautetulla jonolla.
        let task_map = durable_queue.reload().await?;
        let queue = TaskQueue::from_map(task_map);
        let mut pipeline = Pipeline::with_restored_queue(queue);

        // Palauta odottavat hyväksynnät ledgeriin, jotta `approve` löytää ne.
        for record in pending.list()? {
            pipeline.reinstate_approval(record.approval);
        }

        Ok(Self {
            pipeline,
            executors: HashMap::new(),
            proofs: HashMap::new(),
            pending,
            durable_queue: Some(durable_queue),
            rate_limiter: DangerousToolRateLimiter::new(
                DEFAULT_DANGEROUS_TOOL_WINDOW_SECS,
                DEFAULT_DANGEROUS_TOOL_LIMIT,
            ),
            being_id: DEFAULT_BEING_ID.to_string(),
            dispatch_outbox: Box::new(InMemoryDispatchOutbox::new()),
        })
    }

    /// Vaihtaa ajoympäristön **lähetyksen idempotenssi-outboxin** annettuun
    /// toteutukseen ja palauttaa itsensä (builder-tyyli).
    ///
    /// Tämä on at-most-once-takuun kytkentäkohta (kaksoislaukaisun esto, EI
    /// universaali exactly-once completion). Oletus
    /// ([`ActionRuntime::new`]) on muistinvarainen
    /// ([`InMemoryDispatchOutbox`]) eikä selviä kaatumisesta; anna
    /// [`crate::dispatch_outbox::JournalDispatchOutbox`] saadaksesi takuun:
    /// `submit_task`:n sivuvaikutus suoritetaan **korkeintaan kerran** SIGKILL-
    /// kaatumisen yli (ei koskaan kahdesti), ja jo sitoutunut lähetys palautuu
    /// arvo-identtisenä.
    ///
    /// ```
    /// # use familyclaw_actions::ActionRuntime;
    /// # use familyclaw_actions::dispatch_outbox::InMemoryDispatchOutbox;
    /// let runtime = ActionRuntime::new()
    ///     .with_dispatch_outbox(Box::new(InMemoryDispatchOutbox::new()));
    /// let _ = runtime;
    /// ```
    #[must_use]
    pub fn with_dispatch_outbox(mut self, outbox: Box<dyn DispatchOutboxStore>) -> Self {
        self.dispatch_outbox = outbox;
        self
    }

    /// Palauttaa kytketyn lähetys-outboxin **lajitunnisteen** (`"in-memory"` tai
    /// `"journal"`).
    ///
    /// Tämä on salaisuudeton tarkistuskoukku kokoojalle ja testeille: sillä voi
    /// todeta että persistentti kokoonpano sai kaatumiskestävän
    /// (`"journal"`) outboxin oletuksellisen muistinvaraisen (`"in-memory"`)
    /// sijaan, paljastamatta sisäistä tilaa tai tiedostopolkua. Arvo delegoituu
    /// suoraan [`DispatchOutboxStore::kind`]:iin.
    #[must_use]
    pub fn dispatch_outbox_kind(&self) -> &'static str {
        self.dispatch_outbox.kind()
    }

    /// Snapshottaa tehtävän nykytilan kaatumiskestävään jonoon, jos sellainen on
    /// asetettu ([`ActionRuntime::with_durable_stores`]). No-op in-memory-tilassa.
    ///
    /// Best-effort: snapshotin epäonnistuminen **ei** kaada itse toimintoa (se
    /// onnistui jo putkessa), mutta se vaarantaa kaatumiskestävyyden. Palauttaa
    /// `Ok(())` myös no-op-tilassa; kutsuja voi jättää virheen huomiotta tai
    /// propagoida sen. Actions-crate ei riipu lokituskirjastosta, joten virhe
    /// palautetaan eikä logiteta tässä.
    ///
    /// # Errors
    /// [`ActionError::Proof`] jos durable-jonoon kirjoitus epäonnistuu.
    async fn snapshot_task_if_durable(&self, task_id: ActionTaskId) -> Result<()> {
        let Some(durable) = self.durable_queue.as_ref() else {
            return Ok(());
        };
        // Lue tehtävän nykytila putken jonosta ja liitä se durable-lokiin.
        if let Some(task) = self.pipeline.queue().get(task_id).await {
            durable.append(&task).await?;
        }
        Ok(())
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

    /// Lähettää tehtävän annetulle taidolle ja ajaa putken **tämän
    /// ajoympäristön oletusolennon** ([`ActionRuntime::with_being_id`], oletus
    /// `DEFAULT_BEING_ID`) nimissä rate-limit-laskennassa.
    ///
    /// Jos taidon riskiluokka sallii auto-runin, putki suorittaa toiminnon
    /// loppuun ja todiste tallennetaan. Jos käytäntö vaatii ihmisen
    /// hyväksynnän, tehtävä jää tilaan [`TaskStatus::NeedsApproval`] ja
    /// julkisivu **myöntää** payload-sidotun hyväksynnän jonka tunniste
    /// palautetaan ([`SubmitOutcome::pending_approval`]); suorituksen voi
    /// jatkaa [`ActionRuntime::approve`]-kutsulla.
    ///
    /// Kun usea olento jakaa saman ajoympäristön ja kullekin halutaan **oma**
    /// rate-limit-kiintiö, käytä [`ActionRuntime::submit_task_as`]:ia ja anna
    /// olento eksplisiittisesti.
    ///
    /// # Errors
    /// - [`ActionError::UnknownSkill`] jos taitoa ei ole rekisteröity.
    /// - [`ActionError::PolicyDenied`] jos tehtävä vaatisi hyväksynnän mutta
    ///   olento on jo käyttänyt vaarallisten työkalujen rate-limit-kiintiönsä.
    /// - Putken jono-, suoritus- tai todistevirheet.
    pub async fn submit_task(
        &mut self,
        skill_id: SkillId,
        payload: Value,
        now: Timestamp,
    ) -> Result<SubmitOutcome> {
        let being = self.being_id.clone();
        self.submit_task_as(&being, skill_id, payload, now).await
    }

    /// Kuten [`ActionRuntime::submit_task`], mutta lähettää tehtävän
    /// **nimenomaisen olennon** (`being`) nimissä rate-limit-laskennassa.
    ///
    /// Tämä on se kohta jossa vaarallisten (hyväksyntää vaativien)
    /// työkalukutsujen **per-olento-rate-limit** kytkeytyy hyväksyntäpolkuun: jos
    /// putki ratkaisee että tehtävä jää odottamaan ihmisen hyväksyntää, julkisivu
    /// kysyy ensin rajoittimelta ([`DangerousToolRateLimiter::check_and_record`])
    /// onko `being`-olennolla vielä tilaa liukuvassa ikkunassa. Jos kiintiö on
    /// täynnä, hyväksyntää **ei** myönnetä eikä tehtävää jätetä odottamaan —
    /// kutsu hylätään fail-closed ([`ActionError::PolicyDenied`]). Näin yksi
    /// olento ei voi tulvittaa hyväksyntöjen jonoa, vaikka globaali
    /// kapasiteettikatto ei vielä täyttyisi.
    ///
    /// **Auto-run-tehtäviä** (luku / paikallinen kirjoitus, jotka eivät vaadi
    /// hyväksyntää) **ei** rate-limititä: ne suorittuvat normaalisti loppuun,
    /// koska ne eivät kasvata hyväksyntöjen jonoa. Rate-limit kohdistuu
    /// täsmälleen ja vain hyväksyntää vaativiin toimintoihin.
    ///
    /// # Errors
    /// - [`ActionError::UnknownSkill`] jos taitoa ei ole rekisteröity.
    /// - [`ActionError::PolicyDenied`] jos tehtävä vaatisi hyväksynnän mutta
    ///   `being` on jo käyttänyt vaarallisten työkalujen rate-limit-kiintiönsä.
    /// - Putken jono-, suoritus- tai todistevirheet.
    pub async fn submit_task_as(
        &mut self,
        being: &str,
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
            // Per-olento-rate-limit: tarkistetaan ENNEN hyväksynnän myöntämistä.
            // Jos olento on jo täyttänyt kiintiönsä liukuvassa ikkunassa, hylkää
            // fail-closed — hyväksyntää EI myönnetä eikä tehtävää jätetä jonoon
            // odottamaan. Auto-run-tehtävät eivät koskaan päädy tähän haaraan.
            self.rate_limiter.check_and_record(being, now)?;
            let approval = self.pipeline.grant_approval(
                outcome.action_id,
                &payload,
                now,
                Duration::minutes(DEFAULT_APPROVAL_TTL_MINUTES),
            )?;
            let approval_id = approval.id;
            // Redaktoitu tiivistelmä: vain taidon nimi ja tunnisteet — EI raakaa
            // payloadia. Tallennetaan kaatumiskestävälle pinnalle jos sellainen on.
            let summary = self.pending_summary(skill_id);
            let record = PendingRecord::new(task_id, approval, summary, now);
            self.pending.insert(record)?;
            // Kaatumiskestävyys: persistoi tehtävän NeedsApproval-tilannekuva,
            // jotta `approve` löytää tehtävän (payload + tila) myös restartin yli.
            self.snapshot_task_if_durable(task_id).await?;
            Some(approval_id)
        } else {
            // Auto-run-tehtävä eteni loppuun — snapshot durable-jonoon jos asetettu
            // (Done-tila), jottei jää roikkumaan NeedsApproval-rivinä restartissa.
            self.snapshot_task_if_durable(task_id).await?;
            None
        };

        Ok(SubmitOutcome {
            task_id,
            status: outcome.status,
            pending_approval,
        })
    }

    /// Lähettää tehtävän **idempotentisti** kutsujan johtaman vakaan avaimen
    /// (`key`) suojassa — at-most-once-takuun kivijalka (sivuvaikutus lähetetään
    /// **korkeintaan kerran**, ei koskaan kahdesti; tämä on kaksoislaukaisun esto
    /// kaatumisen yli, EI lupaus universaalista exactly-once *valmistumisesta*).
    ///
    /// Tämä on [`ActionRuntime::submit_task_as`]:n kaatumiskestävä kääre. Se
    /// sulkee ikkunan sivuvaikutuksen suorituksen ja sen journaloinnin välissä:
    /// kun sama avain nähdään uudelleen (agenttikerroksen replay tai prosessin
    /// restart), lähetys **ei suorita sivuvaikutusta uudelleen** vaan palauttaa
    /// aiemman lopputuloksen arvo-identtisenä (sama `task_id` / `ApprovalId`).
    ///
    /// ## Kaksivaiheinen sitoutuminen outboxiin
    /// 1. **lookup(key)** — jos avain on jo:
    ///    - **committed** → palauta tallennettu lopputulos heti, ÄLÄ aja
    ///      sivuvaikutusta.
    ///    - **in-progress** (intent kirjattu, committed ei) → prosessi kaatui
    ///      kesken aiemman sivuvaikutuksen. Palautusperiaate on **eksplisiittinen
    ///      ja fail-closed** ([`ActionError::PolicyDenied`]): kutsua EI ajeta
    ///      uudelleen, koska sivuvaikutus on voinut tapahtua osittain.
    ///    - **not-started** → jatka.
    /// 2. **`record_intent`** — kirjaa aie outboxiin (fsync) ENNEN sivuvaikutusta.
    /// 3. aja sivuvaikutus ([`ActionRuntime::submit_task_as`]).
    /// 4. **`record_committed`** — kirjaa lopputulos outboxiin sen
    ///    jälkeen (fsync). Vasta tämä tekee lähetyksestä replay-palautuvan.
    ///
    /// `submit_task`:n virhe tallennetaan committed-rivinä virheenä, jotta sekin
    /// palautuu samana eikä aja sivuvaikutusta uudelleen.
    ///
    /// ## Takuun raja (rehellisesti)
    /// Taattu prosessin kaatumisen / SIGKILL:n yli kun outbox on kaatumiskestävä
    /// ([`crate::dispatch_outbox::JournalDispatchOutbox`]). Muistinvaraisella
    /// oletus-outboxilla takuu kattaa vain saman prosessin sisäisen replayn (ei
    /// restartia). Power-loss / hakemiston metadata-fsync -takuu on yhtä vahva
    /// kuin alla oleva tiedostojärjestelmä — sitä ei yliluvata.
    ///
    /// # Errors
    /// - [`ActionError::PolicyDenied`] jos avain on jäänyt kesken (in-progress)
    ///   aiemmassa kaatumisessa.
    /// - [`ActionError::ExecutionFailed`] jos tallennettu (committed) lähetys oli
    ///   virhe (replay-palautus).
    /// - [`ActionError::Proof`] jos outboxin luku/kirjoitus epäonnistuu.
    /// - [`ActionRuntime::submit_task_as`]:n virheet tuoreessa ajossa.
    pub async fn submit_task_idempotent(
        &mut self,
        key: &str,
        being: &str,
        skill_id: SkillId,
        payload: Value,
        now: Timestamp,
    ) -> Result<SubmitOutcome> {
        // 1) Idempotenssi-tarkistus: onko avain jo aloitettu/sitoutunut?
        match self.dispatch_outbox.lookup(key)? {
            DispatchLookup::Committed(outcome) => {
                // Jo sitoutunut → palauta arvo-identtinen lopputulos ajamatta
                // sivuvaikutusta uudelleen. TÄMÄ on double-firen sulkeva haara.
                return outcome.into_result();
            }
            DispatchLookup::InProgress => {
                // Aie kirjattu mutta ei sitoutumista → kaatui kesken sivuvaikutuksen.
                // Fail-closed: älä aja uudelleen (sivuvaikutus voi olla osittainen).
                return Err(ActionError::PolicyDenied(format!(
                    "lähetys '{key}' jäi kesken aiemmassa kaatumisessa (intent ilman \
                     committed) — ei ajeta uudelleen kaksoislaukaisun estämiseksi"
                )));
            }
            DispatchLookup::NotStarted => {}
        }

        // 2) Kirjaa AIE ENNEN sivuvaikutusta (fsync kaatumiskestävällä outboxilla).
        self.dispatch_outbox.record_intent(key)?;

        // 3) Suorita sivuvaikutus tasan kerran.
        let result = self.submit_task_as(being, skill_id, payload, now).await;

        // 4) Kirjaa SITOUTUMINEN sivuvaikutuksen jälkeen — onnistui tai virhe.
        //    Virhetapaus tallennetaan committed-virheenä, jotta replay palauttaa
        //    saman virheen ajamatta sivuvaikutusta uudelleen (ei kaksoislaukaisua
        //    osittain edenneestä lähetyksestä).
        match &result {
            Ok(outcome) => {
                self.dispatch_outbox
                    .record_committed(key, &DispatchedOutcome::from_submit(outcome))?;
            }
            Err(e) => {
                self.dispatch_outbox
                    .record_committed(key, &DispatchedOutcome::from_error(e.to_string()))?;
            }
        }

        result
    }

    /// Johtaa hyväksynnän **vakaan idempotenssi-avaimen** lähetys-outboxia varten.
    ///
    /// Avain on deterministinen ja **pysyvä yli restartin**: `ApprovalId` on
    /// kaatumiskestävässä tallennuspinnassa, joten sama hyväksyntä tuottaa aina
    /// saman avaimen. Tämä on se mekanismi jolla [`ActionRuntime::approve`]:n
    /// sivuvaikutus lähetetään **korkeintaan kerran** prosessin kaatumisen yli.
    #[must_use]
    fn approval_dispatch_key(approval_id: ApprovalId) -> String {
        format!("approval-{approval_id}")
    }

    /// Kuluttaa (merkitsee käytetyksi) odottavan hyväksynnän ja ajaa pysähtyneen
    /// tehtävän suorituksen loppuun — **idempotentisti** lähetys-outboxin
    /// suojassa (at-most-once-takuun kivijalka hyväksyntäpolulla).
    ///
    /// Hyväksyntä kulutetaan tehtävän tallennettua payloadia vasten
    /// (payload-sidonta + kertakäyttö), joten muutettu payload ei voi käyttää
    /// hyväksyntää. Onnistuessa syntyvä todiste tallennetaan haettavaksi.
    ///
    /// ## Miksi outbox myös tällä polulla (kaksoislaukaisun esto)
    /// Sivuvaikutuksen suoritus ([`Pipeline::run_after_approval`]) ja sen
    /// kuluttavan kirjauksen ([`PendingApprovalStore::remove`]) **väliin** jää
    /// ikkuna: jos prosessi tapetaan (SIGKILL) juuri siinä, sivuvaikutus on jo
    /// tapahtunut mutta hyväksyntä on yhä `pending` kaatumiskestävällä pinnalla →
    /// restartin jälkeen operaattori voi **uudelleenhyväksyä saman hyväksynnän** ja
    /// sivuvaikutus **laukeaisi kahdesti**. Outbox sulkee tämän:
    /// sivuvaikutus kääritään vakaan avaimen (`approval-{id}`) idempotenssiin
    /// täsmälleen kuten [`ActionRuntime::submit_task_idempotent`]:ssa, joten
    /// uudelleenhyväksyntä osuu outboxiin eikä aja sivuvaikutusta uudelleen.
    ///
    /// ## Kaksivaiheinen sitoutuminen outboxiin
    /// 1. **lookup(key)** — jos avain on jo:
    ///    - **committed** → palauta tallennettu lopputulos heti, ÄLÄ aja
    ///      sivuvaikutusta uudelleen.
    ///    - **in-progress** (intent kirjattu, committed ei) → prosessi kaatui
    ///      kesken aiemman sivuvaikutuksen → **fail-closed**
    ///      ([`ActionError::PolicyDenied`]), ÄLÄ aja uudelleen.
    ///    - **not-started** → jatka.
    /// 2. **`record_intent`** (fsync) ENNEN sivuvaikutusta.
    /// 3. aja sivuvaikutus ([`Pipeline::run_after_approval`]).
    /// 4. **`record_committed`** (fsync) sivuvaikutuksen jälkeen — vasta tämä
    ///    tekee lähetyksestä replay-palautuvan.
    ///
    /// `pending.remove` + tilannevedos seuraavat committedin jälkeen, mutta ovat
    /// nyt idempotenssin suojaamia: uudelleenhyväksyntä ei aja sivuvaikutusta
    /// uudelleen.
    ///
    /// ## Takuun raja (rehellisesti)
    /// Tämä on kaksoislaukaisun esto / **at-most-once-lähetys** kaatumisen yli
    /// (fail-closed intent-only-ikkunassa) — **EI** lupaus universaalista
    /// exactly-once-*valmistumisesta*. Takuu kattaa SIGKILL:n vain
    /// kaatumiskestävällä outboxilla ([`crate::dispatch_outbox::JournalDispatchOutbox`]);
    /// muistinvaraisella oletus-outboxilla käyttäytyminen on ennallaan (vain saman
    /// prosessin sisäinen replay, ei restartia).
    ///
    /// # Errors
    /// - [`ActionError::ApprovalMissing`] jos hyväksyntää ei ole odottamassa.
    /// - [`ActionError::UnknownSkill`] jos tehtävän taitoa ei (enää) löydy.
    /// - [`ActionError::PolicyDenied`] jos hyväksyntä on jäänyt kesken
    ///   (intent-only) aiemmassa kaatumisessa.
    /// - [`ActionError::ExecutionFailed`] jos tallennettu (committed) lähetys oli
    ///   virhe (replay-palautus).
    /// - [`ActionError::Proof`] jos outboxin luku/kirjoitus epäonnistuu.
    /// - Hyväksynnän kulutuksen tai putken virheet
    ///   ([`Pipeline::run_after_approval`]).
    pub async fn approve(
        &mut self,
        approval_id: ApprovalId,
        now: Timestamp,
    ) -> Result<SubmitOutcome> {
        let entry = self
            .pending
            .get(approval_id)?
            .ok_or_else(|| ActionError::ApprovalMissing(approval_id.to_string()))?;

        // Vakaa idempotenssi-avain: pysyy samana yli restartin (ApprovalId on
        // kaatumiskestävällä pinnalla). Sama outbox-protokolla kuin
        // `submit_task_idempotent`:ssa.
        let key = Self::approval_dispatch_key(approval_id);

        // 1) Idempotenssi-tarkistus ENNEN sivuvaikutusta.
        match self.dispatch_outbox.lookup(&key)? {
            DispatchLookup::Committed(outcome) => {
                // Jo sitoutunut → palauta arvo-identtinen lopputulos ajamatta
                // sivuvaikutusta uudelleen. TÄMÄ on double-firen sulkeva haara
                // uudelleenhyväksynnän yli.
                return outcome.into_result();
            }
            DispatchLookup::InProgress => {
                // Intent kirjattu mutta ei committed → kaatui kesken sivuvaikutuksen.
                // Fail-closed: älä aja uudelleen (sivuvaikutus voi olla osittainen).
                return Err(ActionError::PolicyDenied(format!(
                    "hyväksynnän '{approval_id}' lähetys jäi kesken aiemmassa \
                     kaatumisessa (intent ilman committed) — ei ajeta uudelleen \
                     kaksoislaukaisun estämiseksi"
                )));
            }
            DispatchLookup::NotStarted => {}
        }

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

        // 2) Kirjaa AIE ENNEN sivuvaikutusta (fsync kaatumiskestävällä outboxilla).
        self.dispatch_outbox.record_intent(&key)?;

        // 3) Suorita sivuvaikutus (hyväksynnän kulutus + putken ajo) tasan kerran.
        let run_result = self
            .pipeline
            .run_after_approval(executor.as_ref(), entry.task_id, &entry.approval, now)
            .await;

        // 4) Kirjaa SITOUTUMINEN sivuvaikutuksen jälkeen — onnistui tai virhe.
        //    Virhetapaus tallennetaan committed-virheenä, jotta uudelleenhyväksyntä
        //    palauttaa saman virheen ajamatta sivuvaikutusta uudelleen.
        let outcome = match run_result {
            Ok(outcome) => {
                let submit = SubmitOutcome {
                    task_id: entry.task_id,
                    status: outcome.status,
                    pending_approval: None,
                };
                self.dispatch_outbox
                    .record_committed(&key, &DispatchedOutcome::from_submit(&submit))?;
                outcome
            }
            Err(e) => {
                self.dispatch_outbox
                    .record_committed(&key, &DispatchedOutcome::from_error(e.to_string()))?;
                return Err(e);
            }
        };

        // Hyväksyntä on nyt kulutettu — poista se odottavista (pysyvästi, myös
        // kaatumiskestävältä pinnalta). Idempotenssin suojaama: uudelleenhyväksyntä
        // osuu yllä committed-haaraan eikä koskaan päädy tänne uudelleen.
        self.pending.remove(approval_id)?;
        // Kaatumiskestävyys: persistoi tehtävän lopullinen (Done/Failed) tila
        // durable-jonoon, jotta restart ei näe sitä enää NeedsApproval-rivinä.
        self.snapshot_task_if_durable(entry.task_id).await?;

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
    /// vuoksi. Jos tallennuspinnan luku epäonnistuu (esim. levyvirhe
    /// kaatumiskestävällä pinnalla), palautetaan **tyhjä luettelo** — operaattorin
    /// listaus ei koskaan panikoi. Käytä [`ActionRuntime::try_pending_approvals`]:a
    /// jos haluat virheen propagoituvan.
    #[must_use]
    pub fn pending_approvals(&self) -> Vec<PendingApproval> {
        self.try_pending_approvals().unwrap_or_default()
    }

    /// Kuten [`ActionRuntime::pending_approvals`], mutta propagoi tallennuspinnan
    /// lukuvirheen sen sijaan että palauttaisi tyhjän luettelon.
    ///
    /// # Errors
    /// Tallennuspinnan ([`PendingApprovalStore::list`]) lukuvirhe — käytännössä
    /// vain kaatumiskestävällä pinnalla, jos journalia ei voi lukea.
    pub fn try_pending_approvals(&self) -> Result<Vec<PendingApproval>> {
        let mut out: Vec<PendingApproval> = self
            .pending
            .list()?
            .into_iter()
            .map(|record| PendingApproval {
                approval_id: record.approval_id(),
                task_id: record.task_id,
            })
            .collect();
        out.sort_by_key(|a| a.approval_id);
        Ok(out)
    }

    /// Häätää tallennuspinnalta kaikki annettuun hetkeen `now` mennessä
    /// vanhentuneet odottavat hyväksynnät ja palauttaa häädettyjen lukumäärän.
    ///
    /// Käyttää samaa fail-closed-vanhentumisrajaa kuin [`crate::approval`]
    /// (`now > expires_at`). Operaattori voi kutsua tätä jaksoittain pitääkseen
    /// odottavien jonon siistinä; vanhentunutta hyväksyntää ei voi enää kuluttaa.
    ///
    /// # Errors
    /// Tallennuspinnan ([`PendingApprovalStore::evict_expired`]) virhe.
    pub fn evict_expired_approvals(&self, now: Timestamp) -> Result<usize> {
        self.pending.evict_expired(now)
    }

    /// Palauttaa odottavan hyväksynnän **redaktoidun, operaattorille
    /// turvallisen tiivistelmän** tunnisteella; `None` jos hyväksyntää ei (enää)
    /// odoteta tai tallennuspinnan luku epäonnistuu.
    ///
    /// Tämä on se sama merkkijono jonka `submit-task` tallensi odottavaan
    /// kirjaukseen ([`crate::pending_store::PendingRecord::redacted_summary`]) —
    /// johdettu vain taidon nimestä ja tunnisteista, **ei koskaan raakaa
    /// payloadia eikä salaisuuksia**. Sen voi näyttää operaattorille tai
    /// säilyttää resumea varten sellaisenaan.
    ///
    /// Käytetään mm. agenttikerroksen `ThinkOutcome::Suspended`-polulla:
    /// kun työkalu pysähtyy odottamaan hyväksyntää, agentti tallentaa tämän
    /// turvallisen tiivistelmän (+ `approval_id`:n) vuoron durable-tilaan
    /// resumea varten — sen sijaan että vuotaisi raakaa hyväksyntätietoa
    /// reply-putkeen.
    #[must_use]
    pub fn pending_summary_for(&self, approval_id: ApprovalId) -> Option<String> {
        self.pending
            .get(approval_id)
            .ok()
            .flatten()
            .map(|record| record.redacted_summary)
    }

    /// Palauttaa odottavan hyväksynnän **vanhentumishetken**
    /// ([`crate::approval::Approval::expires_at`]) tunnisteella; `None` jos
    /// hyväksyntää ei (enää) odoteta tai tallennuspinnan luku epäonnistuu.
    ///
    /// Tämä on salaisuudeton aikaleima (ei payloadia eikä tiivistettä), jonka
    /// agenttikerros tarvitsee sitoakseen **jatkettavan vuoron**
    /// ([`crate::pending_store::PendingRecord`]:n päälle rakennetun resume-tilan)
    /// TTL:n täsmälleen samaan vanhentumiseen kuin myönnetty hyväksyntä. Näin
    /// jatkettava vuoro vanhenee samalla hetkellä kuin lupa, jolla se voitaisiin
    /// kuluttaa — ei aiemmin eikä myöhemmin.
    #[must_use]
    pub fn pending_expiry_for(&self, approval_id: ApprovalId) -> Option<Timestamp> {
        self.pending
            .get(approval_id)
            .ok()
            .flatten()
            .map(|record| record.expires_at())
    }

    /// Palauttaa odottavan hyväksynnän **luontihetken**
    /// ([`crate::pending_store::PendingRecord::created_at`]) tunnisteella; `None`
    /// jos hyväksyntää ei (enää) odoteta tai tallennuspinnan luku epäonnistuu.
    ///
    /// Tämä on salaisuudeton auditointiaikaleima (ei payloadia, ei tiivistettä,
    /// ei salaisuuksia), jonka operaattoripinta (esim. gatewayn
    /// `GET /approvals/pending`) näyttää kertoakseen **milloin** hyväksyntää on
    /// odotettu. Se vastaa tarkalleen [`PendingApproval`]:n rinnalla näytettävää
    /// metatietoa eikä paljasta mitään siitä **mitä** hyväksyntä koskee yli sen
    /// mitä [`ActionRuntime::pending_summary_for`] jo redaktoidusti kertoo.
    #[must_use]
    pub fn pending_created_at_for(&self, approval_id: ApprovalId) -> Option<Timestamp> {
        self.pending
            .get(approval_id)
            .ok()
            .flatten()
            .map(|record| record.created_at)
    }

    /// Muodostaa odottavalle hyväksynnälle **redaktoidun** tiivistelmän
    /// tallennettavaksi: vain taidon nimi (tai tunniste) — ei koskaan raakaa
    /// payloadia eikä salaisuuksia.
    fn pending_summary(&self, skill_id: SkillId) -> String {
        let name = self
            .pipeline
            .registry()
            .get(&skill_id)
            .map_or_else(|| skill_id.to_string(), |m| m.name.clone());
        format!("taito '{name}' odottaa ihmisen hyväksyntää")
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

    /// Oletuskokoonpano ([`ActionRuntime::with_default_skills`]) saa
    /// muistinvaraisen outboxin, ja [`ActionRuntime::with_dispatch_outbox`]
    /// kytkee kaatumiskestävän journal-variantin tilalle.
    ///
    /// Tämä lukitsee kytkennän kontrollin: kokooja (`familyclaw-runtime`) luottaa
    /// `dispatch_outbox_kind()`:iin todetakseen että persistentti polku sai
    /// `"journal"`-outboxin oletuksellisen `"in-memory"`:n sijaan.
    #[test]
    fn dispatch_outbox_kind_reflects_wired_variant() {
        // Oletus: muistinvarainen.
        let in_memory = ActionRuntime::with_default_skills().expect("default skills");
        assert_eq!(in_memory.dispatch_outbox_kind(), "in-memory");

        // Kytketty journal-outbox → "journal".
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "familyclaw-facade-outbox-{}-{nanos}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let journal = crate::dispatch_outbox::JournalDispatchOutbox::open(&path)
            .expect("open journal outbox");
        let durable = ActionRuntime::with_default_skills()
            .expect("default skills")
            .with_dispatch_outbox(Box::new(journal));
        assert_eq!(durable.dispatch_outbox_kind(), "journal");
        let _ = std::fs::remove_file(&path);
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
    async fn per_being_rate_limit_denies_next_approval_required_submit() {
        // Tiukka rajoitin: korkeintaan 2 hyväksyntää vaativaa toimintoa per olento
        // 60 s ikkunassa. Kolmas saman olennon hyväksyntää vaativa lähetys
        // hylätään fail-closed.
        let mut runtime = ActionRuntime::with_default_skills()
            .expect("default skills")
            .with_rate_limiter(DangerousToolRateLimiter::new(60, 2));
        let now = at(1_700_000_000);
        let payload = json!({ "bug_report": "Button does nothing" });

        // Kaksi ensimmäistä hyväksyntää vaativaa lähetystä mahtuvat kiintiöön.
        let first = runtime
            .submit_task_as("being-a", GithubIssueDraftMock::skill_id(), payload.clone(), now)
            .await
            .expect("first approval-required submit fits quota");
        assert!(first.awaiting_approval(), "first must await approval");
        let second = runtime
            .submit_task_as("being-a", GithubIssueDraftMock::skill_id(), payload.clone(), now)
            .await
            .expect("second approval-required submit fits quota");
        assert!(second.awaiting_approval(), "second must await approval");

        // Kolmas ylittää per-olento-kiintiön → PolicyDenied (hyväksyntää ei myönnetä).
        let err = runtime
            .submit_task_as("being-a", GithubIssueDraftMock::skill_id(), payload, now)
            .await
            .expect_err("third approval-required submit exceeds per-being quota");
        assert!(matches!(err, ActionError::PolicyDenied(_)));

        // Hyväksyntää ei myönnetty kolmannelle → odottavia on yhä vain kaksi.
        assert_eq!(
            runtime.pending_approvals().len(),
            2,
            "denied submit must not enqueue a pending approval"
        );
    }

    #[tokio::test]
    async fn rate_limit_is_per_being_separate_quota() {
        // Rajoitin sallii vain yhden hyväksyntää vaativan toimen per olento per
        // ikkuna. being-a kuluttaa kiintiönsä; being-b on koskematon (oma kiintiö).
        let mut runtime = ActionRuntime::with_default_skills()
            .expect("default skills")
            .with_rate_limiter(DangerousToolRateLimiter::new(60, 1));
        let now = at(1_700_000_000);
        let payload = json!({ "bug_report": "Button does nothing" });

        runtime
            .submit_task_as("being-a", GithubIssueDraftMock::skill_id(), payload.clone(), now)
            .await
            .expect("being-a first fits its quota");
        // being-a on nyt täynnä.
        let denied = runtime
            .submit_task_as("being-a", GithubIssueDraftMock::skill_id(), payload.clone(), now)
            .await
            .expect_err("being-a second exceeds quota");
        assert!(matches!(denied, ActionError::PolicyDenied(_)));

        // ERI olento → oma kiintiö, ei vaikutusta being-a:n täyttymisestä.
        let other = runtime
            .submit_task_as("being-b", GithubIssueDraftMock::skill_id(), payload, now)
            .await
            .expect("being-b unaffected by being-a quota");
        assert!(other.awaiting_approval(), "being-b must still get approval");
    }

    #[tokio::test]
    async fn rate_limit_window_slides_capacity_returns() {
        // Yksi hyväksyntää vaativa toimi per 60 s ikkuna. Ikkunan jälkeen kiintiö
        // palautuu ja sama olento saa taas lähettää.
        let mut runtime = ActionRuntime::with_default_skills()
            .expect("default skills")
            .with_rate_limiter(DangerousToolRateLimiter::new(60, 1));
        let now = at(1_700_000_000);
        let payload = json!({ "bug_report": "Button does nothing" });

        runtime
            .submit_task_as("being-a", GithubIssueDraftMock::skill_id(), payload.clone(), now)
            .await
            .expect("first fits quota");
        // Heti perään sama ikkuna → estetty.
        let denied = runtime
            .submit_task_as("being-a", GithubIssueDraftMock::skill_id(), payload.clone(), now)
            .await
            .expect_err("second in same window is denied");
        assert!(matches!(denied, ActionError::PolicyDenied(_)));

        // Ikkunan liu'uttua (now + 61 s) vanha kirjaus häätyy → tilaa taas.
        let later = at(1_700_000_061);
        let after = runtime
            .submit_task_as("being-a", GithubIssueDraftMock::skill_id(), payload, later)
            .await
            .expect("capacity returns after window slides");
        assert!(after.awaiting_approval(), "submit must succeed after window");
    }

    #[tokio::test]
    async fn auto_run_tasks_are_not_rate_limited() {
        // Rajoitin joka estäisi kaikki vaaralliset kutsut (kiintiö 0). Read-only
        // (auto-run) -tehtävät EIVÄT mene rate-limitin läpi → suorittuvat aina.
        let mut runtime = ActionRuntime::with_default_skills()
            .expect("default skills")
            .with_rate_limiter(DangerousToolRateLimiter::new(60, 0));
        let now = at(1_700_000_000);
        let payload = json!({
            "emails": [
                { "from": "user@example.com", "subject": "Invoice question", "body": "When is it due?" }
            ]
        });

        // Useita peräkkäisiä read-only-lähetyksiä — kiintiö 0 ei estä yhtäkään,
        // koska ne eivät vaadi hyväksyntää eivätkä kosketa rate-limiteria.
        for _ in 0..3 {
            let outcome = runtime
                .submit_task_as("being-a", EmailTriageMock::skill_id(), payload.clone(), now)
                .await
                .expect("read-only auto-run is never rate-limited");
            assert_eq!(outcome.status, TaskStatus::Done);
            assert!(!outcome.awaiting_approval());
        }
        // Yksikään ei jäänyt odottamaan hyväksyntää.
        assert!(runtime.pending_approvals().is_empty());
    }

    #[tokio::test]
    async fn pending_created_at_for_returns_record_creation_time() {
        // Odottava hyväksyntä → luontihetki on haettavissa tunnisteella ja
        // vastaa `submit_task`:lle annettua `now`-aikaleimaa (deterministinen).
        let mut runtime = ActionRuntime::with_default_skills().expect("default skills");
        let now = at(1_700_000_000);
        let submitted = runtime
            .submit_task(
                GithubIssueDraftMock::skill_id(),
                json!({ "bug_report": "Button does nothing" }),
                now,
            )
            .await
            .expect("submit");
        let approval_id = submitted.pending_approval.expect("approval granted");

        assert_eq!(runtime.pending_created_at_for(approval_id), Some(now));
        // Tuntematon tunniste → None (fail-closed, ei paniikkia).
        assert!(runtime.pending_created_at_for(ApprovalId::new()).is_none());
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

    // --- approve()-idempotenssi (kaksoislaukaisun esto hyväksyntäpolulla) ---

    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    /// Testitaito joka **vaatii hyväksynnän** (write-external) ja laskee
    /// sivuvaikutuksen ajot. Käytetään todistamaan, että [`ActionRuntime::approve`]
    /// ajaa sivuvaikutuksen **korkeintaan kerran** outbox-suojan alla.
    #[derive(Debug, Clone)]
    struct CountingApprovalSkill {
        /// Sivuvaikutuksen ajojen lukumäärä (jaettu testin kanssa).
        runs: Arc<AtomicU64>,
    }

    /// Laskevan testitaidon kiinteä tunniste.
    const COUNTING_SKILL_UUID: uuid::Uuid = uuid::uuid!("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");

    #[async_trait::async_trait]
    impl ActionExecutor for CountingApprovalSkill {
        async fn execute(
            &self,
            request: crate::executor::ActionRequest,
        ) -> Result<crate::executor::ActionResult> {
            // SIVUVAIKUTUS: kasvata laskuria. Tämän on tapahduttava tasan kerran.
            self.runs.fetch_add(1, Ordering::SeqCst);
            Ok(crate::executor::ActionResult::success(
                "side effect fired",
                json!({ "ok": true }),
                request.now,
            ))
        }
    }

    impl Skill for CountingApprovalSkill {
        fn manifest(&self) -> crate::manifest::SkillManifest {
            crate::manifest::SkillManifest {
                id: SkillId::from_uuid(COUNTING_SKILL_UUID),
                name: "counting_approval_test".to_string(),
                version: "1.0.0".to_string(),
                description: "Laskee sivuvaikutuksen ajot (vaatii hyväksynnän, testikäyttö)."
                    .to_string(),
                // Write-external → vaatii hyväksynnän → kulkee run_after_approval-polkua.
                permissions: vec![crate::policy::SkillPermission::WriteExternal],
                risk: ActionRisk::WriteExternal,
                approval_policy: crate::policy::ApprovalPolicy::RequireApproval,
                input_hint: None,
                output_hint: None,
                input_schema: crate::manifest::default_input_schema(),
            }
        }
    }

    /// Jaettu (Arc-taustainen) muistinvarainen outbox testin esiseedausta varten.
    ///
    /// [`ActionRuntime::with_dispatch_outbox`] kuluttaa `Box<dyn ...>`:n, joten
    /// tämä kääre antaa testille rinnakkaisen kahvan samaan outbox-tilaan: testi
    /// voi kirjata committed/intent-rivin ENNEN `approve`-kutsua ja todeta, ettei
    /// sivuvaikutus aja uudelleen.
    #[derive(Debug, Clone)]
    struct SharedOutbox(Arc<InMemoryDispatchOutbox>);

    impl DispatchOutboxStore for SharedOutbox {
        fn kind(&self) -> &'static str {
            self.0.kind()
        }
        fn lookup(&self, key: &str) -> Result<DispatchLookup> {
            self.0.lookup(key)
        }
        fn record_intent(&self, key: &str) -> Result<()> {
            self.0.record_intent(key)
        }
        fn record_committed(&self, key: &str, outcome: &DispatchedOutcome) -> Result<()> {
            self.0.record_committed(key, outcome)
        }
    }

    /// Rakentaa ajoympäristön laskevalla hyväksyntätaidolla + jaetulla outboxilla,
    /// ja lähettää yhden hyväksyntää vaativan tehtävän. Palauttaa runtimen, jaetun
    /// outbox-kahvan, laskurin, lähetetyn tehtävän tunnisteen ja `approval_id`:n.
    async fn build_approval_fixture(
        now: Timestamp,
    ) -> (
        ActionRuntime,
        Arc<InMemoryDispatchOutbox>,
        Arc<AtomicU64>,
        ActionTaskId,
        ApprovalId,
    ) {
        let runs = Arc::new(AtomicU64::new(0));
        let shared = Arc::new(InMemoryDispatchOutbox::new());
        let mut runtime = ActionRuntime::new().with_dispatch_outbox(Box::new(SharedOutbox(
            Arc::clone(&shared),
        )));
        runtime
            .register_skill(CountingApprovalSkill {
                runs: Arc::clone(&runs),
            })
            .expect("register counting approval skill");

        let submitted = runtime
            .submit_task(
                SkillId::from_uuid(COUNTING_SKILL_UUID),
                json!({ "any": "payload" }),
                now,
            )
            .await
            .expect("submit");
        assert_eq!(submitted.status, TaskStatus::NeedsApproval);
        let approval_id = submitted.pending_approval.expect("approval granted");
        // Lähetys ei ole vielä ajanut sivuvaikutusta (odottaa hyväksyntää).
        assert_eq!(runs.load(Ordering::SeqCst), 0, "no side effect before approve");

        (runtime, shared, runs, submitted.task_id, approval_id)
    }

    #[tokio::test]
    async fn approve_with_committed_outbox_entry_returns_prior_without_rerun() {
        // Skenaario: prosessi kaatui aiemmin `record_committed`:n JÄLKEEN mutta
        // `pending.remove`:n EDELLÄ → hyväksyntä on yhä odottavissa, mutta outboxissa
        // on committed-rivi avaimelle `approval-{id}`. Uudelleenhyväksyntä EI saa
        // ajaa sivuvaikutusta uudelleen, vaan palauttaa tallennetun lopputuloksen.
        let now = at(1_700_000_000);
        let (mut runtime, shared, runs, task_id, approval_id) =
            build_approval_fixture(now).await;

        // Esiseedaa outbox committed-rivillä TÄSMÄLLEEN avaimelle approve käyttää.
        let key = ActionRuntime::approval_dispatch_key(approval_id);
        let prior = DispatchedOutcome {
            task_id,
            status: TaskStatus::Done,
            pending_approval: None,
            error: None,
        };
        shared.record_committed(&key, &prior).expect("seed committed");

        // approve → committed-haara: palauttaa aiemman lopputuloksen ajamatta.
        let approved = runtime.approve(approval_id, now).await.expect("approve");
        assert_eq!(approved.task_id, task_id);
        assert_eq!(approved.status, TaskStatus::Done);
        assert!(approved.pending_approval.is_none());
        // KRIITTINEN: laskuri pysyy 0:ssa — sivuvaikutus EI ajanut uudelleen.
        assert_eq!(
            runs.load(Ordering::SeqCst),
            0,
            "committed outbox entry must short-circuit before run_after_approval"
        );
    }

    #[tokio::test]
    async fn approve_with_intent_only_outbox_entry_fails_closed_without_rerun() {
        // Skenaario: prosessi kaatui intent-only-ikkunassa (intent levyllä,
        // committed kirjoittamatta, sivuvaikutus mahdollisesti osittain ajanut).
        // Uudelleenhyväksyntä on fail-closed (PolicyDenied) eikä aja uudelleen.
        let now = at(1_700_000_000);
        let (mut runtime, shared, runs, _task_id, approval_id) =
            build_approval_fixture(now).await;

        // Esiseedaa outbox VAIN intent-rivillä (ei committed) → InProgress.
        let key = ActionRuntime::approval_dispatch_key(approval_id);
        shared.record_intent(&key).expect("seed intent");

        // approve → in-progress-haara: fail-closed PolicyDenied, ei sivuvaikutusta.
        let before = runs.load(Ordering::SeqCst);
        let err = runtime
            .approve(approval_id, now)
            .await
            .expect_err("intent-only must fail closed");
        assert!(
            matches!(err, ActionError::PolicyDenied(_)),
            "intent-only window must be PolicyDenied (fail-closed), got {err:?}"
        );
        // Laskuri pysyy ennallaan — sivuvaikutus EI ajanut uudelleen.
        assert_eq!(
            runs.load(Ordering::SeqCst),
            before,
            "intent-only outbox entry must not re-run the side effect"
        );
    }
}
