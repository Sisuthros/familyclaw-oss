//! Agenttien väliset sopimukset: tyypitetty FIPA-ContractNet-toteutus.
//!
//! Tämä moduuli antaa agenteille **todennettavan** tavan sopia työstä:
//! palveluntarjoaja mainostaa [`Capability`]-kyvyn (jolla on tyypitetty
//! syöte-/tulosskeema ja esi-/jälkiehdot), pyytäjä tekee sopimusehdotuksen
//! ([`ContractBoard::propose`]) joka validoidaan syöteskeemaa vasten, ja
//! sopimus täytetään ([`ContractBoard::fulfill`]) vasta kun toimite läpäisee
//! tulosskeeman ja **jokaisen** jälkiehdon.
//!
//! ## Miksi tyypitetty?
//! Pelkkä "tee tämä" ei riitä luotettavaan moniagenttityöhön: tarvitaan
//! koneellisesti tarkistettava lupaus. [`Schema`] tarkistaa rakenteen,
//! [`Clause`] tarkistaa väitteet ("kenttä X on olemassa", "lista ei ole
//! tyhjä", "arvo ≥ N"). Jälkiehtojen rikkominen siirtää sopimuksen tilaan
//! [`ContractStatus::Failed`] virheen kanssa — ei hiljaista hyväksyntää.
//!
//! ## OSS-raja
//! Geneerinen: ei kovakoodattuja kykyjä, sieluja eikä avaimia. Hyötykuormat
//! ovat `serde_json::Value`.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::RwLock;

use familyclaw_core::ids::{AgentId, MessageId};
use familyclaw_core::time::Timestamp;
use familyclaw_core::FamilyClawError;

use crate::task::TaskId;

// ===========================================================================
// Skeema ja kentät
// ===========================================================================

/// Yksittäisen kentän odotettu tyyppi skeemassa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    /// Merkkijono (`string`).
    Str,
    /// Kokonais- tai liukuluku (`number`).
    Int,
    /// Totuusarvo (`bool`).
    Bool,
    /// Lista (`array`).
    Arr,
    /// Objekti (`object`).
    Obj,
}

impl FieldType {
    /// Täsmääkö annettu JSON-arvo tähän tyyppiin.
    #[must_use]
    pub fn matches(self, value: &Value) -> bool {
        match self {
            FieldType::Str => value.is_string(),
            // `Int` hyväksyy minkä tahansa JSON-numeron (kokonais/liuku).
            FieldType::Int => value.is_number(),
            FieldType::Bool => value.is_boolean(),
            FieldType::Arr => value.is_array(),
            FieldType::Obj => value.is_object(),
        }
    }

    /// Tyypin vakaa nimi virheviesteihin.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            FieldType::Str => "string",
            FieldType::Int => "number",
            FieldType::Bool => "bool",
            FieldType::Arr => "array",
            FieldType::Obj => "object",
        }
    }
}

/// Yksittäinen kenttäkuvaus skeemassa.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// Kentän nimi (avain objektissa).
    pub name: String,

    /// Kentän odotettu tyyppi.
    pub ty: FieldType,

    /// Onko kenttä pakollinen. Pakollinen puuttuva kenttä on rikkomus;
    /// valinnaisen puuttuminen on sallittua, mutta jos arvo on annettu, sen
    /// tyypin on täsmättävä.
    #[serde(default = "default_true")]
    pub required: bool,
}

/// Serde-oletus `required`-kentälle (`true`).
const fn default_true() -> bool {
    true
}

impl Field {
    /// Pakollinen kenttä.
    pub fn required(name: impl Into<String>, ty: FieldType) -> Self {
        Self {
            name: name.into(),
            ty,
            required: true,
        }
    }

    /// Valinnainen kenttä.
    pub fn optional(name: impl Into<String>, ty: FieldType) -> Self {
        Self {
            name: name.into(),
            ty,
            required: false,
        }
    }
}

/// Yhden skeemarikkomuksen kuvaus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaViolation {
    /// Kentän nimi johon rikkomus liittyy.
    pub field: String,

    /// Ihmisluettava syy (esim. "missing required field", "expected number").
    pub reason: String,
}

/// Tyypitetty objektiskeema: joukko nimettyjä kenttiä.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schema {
    /// Skeeman kentät.
    pub fields: Vec<Field>,
}

impl Schema {
    /// Rakentaa skeeman kenttälistasta.
    #[must_use]
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields }
    }

    /// Tyhjä skeema (hyväksyy minkä tahansa objektin).
    #[must_use]
    pub fn empty() -> Self {
        Self { fields: Vec::new() }
    }

    /// Tarkistaa arvon skeemaa vasten ja palauttaa kaikki rikkomukset.
    ///
    /// Tyhjä palautus tarkoittaa että arvo läpäisi. Jos arvo ei ole objekti
    /// lainkaan, palautetaan yksi rikkomus pseudokentälle `"$root"`.
    #[must_use]
    pub fn check(&self, value: &Value) -> Vec<SchemaViolation> {
        let mut out = Vec::new();
        let Some(obj) = value.as_object() else {
            out.push(SchemaViolation {
                field: "$root".to_string(),
                reason: "expected object".to_string(),
            });
            return out;
        };
        for field in &self.fields {
            match obj.get(&field.name) {
                None => {
                    if field.required {
                        out.push(SchemaViolation {
                            field: field.name.clone(),
                            reason: "missing required field".to_string(),
                        });
                    }
                }
                Some(Value::Null) if field.required => {
                    out.push(SchemaViolation {
                        field: field.name.clone(),
                        reason: "required field is null".to_string(),
                    });
                }
                Some(v) => {
                    if !field.ty.matches(v) {
                        out.push(SchemaViolation {
                            field: field.name.clone(),
                            reason: format!("expected {}", field.ty.as_str()),
                        });
                    }
                }
            }
        }
        out
    }

    /// Onko arvo skeeman mukainen (ei rikkomuksia).
    #[must_use]
    pub fn is_valid(&self, value: &Value) -> bool {
        self.check(value).is_empty()
    }
}

// ===========================================================================
// Ehtolauseet (Clause)
// ===========================================================================

/// Ehtolauseen operaattori.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClauseOp {
    /// Kenttä on olemassa eikä ole `null`.
    Present,
    /// Kenttä on olemassa eikä ole tyhjä (merkkijono/lista/objekti).
    NonEmpty,
    /// Kentän arvo on yhtä suuri kuin vertailuarvo.
    Eq,
    /// Kentän numeerinen arvo on ≥ vertailuarvo.
    Gte,
    /// Kentän numeerinen arvo on ≤ vertailuarvo.
    Lte,
    /// Kentän pituus (merkkijono/lista/objekti) on ≥ vertailuarvo.
    MinLen,
    /// Kentän pituus (merkkijono/lista/objekti) on ≤ vertailuarvo.
    MaxLen,
}

/// Yksittäinen ehtolause: väite kentästä toimitteessa/syötteessä.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clause {
    /// Tarkistettavan kentän nimi.
    pub field: String,

    /// Operaattori.
    pub op: ClauseOp,

    /// Vertailuarvo (operaattorista riippuen luku, merkkijono jne.).
    /// `Present`/`NonEmpty` jättävät tämän huomiotta.
    #[serde(default)]
    pub value: Value,
}

impl Clause {
    /// `field` on olemassa eikä `null`.
    pub fn present(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::Present,
            value: Value::Null,
        }
    }

    /// `field` ei ole tyhjä.
    pub fn non_empty(field: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::NonEmpty,
            value: Value::Null,
        }
    }

    /// `field == value`.
    pub fn eq(field: impl Into<String>, value: Value) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::Eq,
            value,
        }
    }

    /// `field >= value` (numeerinen).
    pub fn gte(field: impl Into<String>, value: Value) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::Gte,
            value,
        }
    }

    /// `field <= value` (numeerinen).
    pub fn lte(field: impl Into<String>, value: Value) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::Lte,
            value,
        }
    }

    /// `len(field) >= value`.
    pub fn min_len(field: impl Into<String>, value: u64) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::MinLen,
            value: Value::from(value),
        }
    }

    /// `len(field) <= value`.
    pub fn max_len(field: impl Into<String>, value: u64) -> Self {
        Self {
            field: field.into(),
            op: ClauseOp::MaxLen,
            value: Value::from(value),
        }
    }

    /// Ihmisluettava kuvaus ehdosta (lokiin ja virheviesteihin).
    #[must_use]
    pub fn describe(&self) -> String {
        match self.op {
            ClauseOp::Present => format!("{} present", self.field),
            ClauseOp::NonEmpty => format!("{} non-empty", self.field),
            ClauseOp::Eq => format!("{} == {}", self.field, self.value),
            ClauseOp::Gte => format!("{} >= {}", self.field, self.value),
            ClauseOp::Lte => format!("{} <= {}", self.field, self.value),
            ClauseOp::MinLen => format!("len({}) >= {}", self.field, self.value),
            ClauseOp::MaxLen => format!("len({}) <= {}", self.field, self.value),
        }
    }

    /// Arvioi ehdon annettua (objekti)arvoa vasten.
    ///
    /// Palauttaa `false` jos kenttää ei ole, tyyppi ei sovi operaattorille,
    /// tai väite ei pidä paikkaansa.
    #[must_use]
    pub fn eval(&self, value: &Value) -> bool {
        let field = value.get(&self.field);
        match self.op {
            ClauseOp::Present => matches!(field, Some(v) if !v.is_null()),
            ClauseOp::NonEmpty => match field {
                Some(Value::String(s)) => !s.is_empty(),
                Some(Value::Array(a)) => !a.is_empty(),
                Some(Value::Object(o)) => !o.is_empty(),
                _ => false,
            },
            ClauseOp::Eq => field == Some(&self.value),
            ClauseOp::Gte => match (number(field), number(Some(&self.value))) {
                (Some(a), Some(b)) => a >= b,
                _ => false,
            },
            ClauseOp::Lte => match (number(field), number(Some(&self.value))) {
                (Some(a), Some(b)) => a <= b,
                _ => false,
            },
            ClauseOp::MinLen => match (length(field), self.value.as_u64()) {
                (Some(len), Some(min)) => len >= min,
                _ => false,
            },
            ClauseOp::MaxLen => match (length(field), self.value.as_u64()) {
                (Some(len), Some(max)) => len <= max,
                _ => false,
            },
        }
    }
}

/// Poimii numeerisen arvon (f64) JSON-arvosta, jos se on numero.
fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(Value::as_f64)
}

/// Palauttaa kentän pituuden (merkkijono/lista/objekti), jos sovellettavissa.
fn length(value: Option<&Value>) -> Option<u64> {
    match value {
        Some(Value::String(s)) => Some(s.chars().count() as u64),
        Some(Value::Array(a)) => Some(a.len() as u64),
        Some(Value::Object(o)) => Some(o.len() as u64),
        _ => None,
    }
}

// ===========================================================================
// Kyvykkyys ja sen rekisteri
// ===========================================================================

/// Tyypitetty kyvykkyys jonka palveluntarjoaja voi mainostaa.
///
/// Sisältää syöte-/tulosskeeman sekä esi- ja jälkiehdot. Esiehdot tarkistetaan
/// kun sopimus hyväksytään; jälkiehdot kun se täytetään.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    /// Kyvyn vakaa tunniste.
    pub id: MessageId,

    /// Kyvyn nimi (esim. `"render_video"`).
    pub name: String,

    /// Syötteen skeema.
    pub input: Schema,

    /// Tuloksen skeema.
    pub output: Schema,

    /// Esiehdot (tarkistetaan hyväksynnässä, syötettä vasten).
    #[serde(default)]
    pub preconditions: Vec<Clause>,

    /// Jälkiehdot (tarkistetaan täyttämisessä, toimitetta vasten).
    #[serde(default)]
    pub postconditions: Vec<Clause>,
}

impl Capability {
    /// Rakentaa kyvyn nimellä ja skeemoilla, ilman ehtoja.
    pub fn new(name: impl Into<String>, input: Schema, output: Schema) -> Self {
        Self {
            id: MessageId::new(),
            name: name.into(),
            input,
            output,
            preconditions: Vec::new(),
            postconditions: Vec::new(),
        }
    }

    /// Asettaa esiehdot (builder-tyyli).
    #[must_use]
    pub fn with_preconditions(mut self, clauses: Vec<Clause>) -> Self {
        self.preconditions = clauses;
        self
    }

    /// Asettaa jälkiehdot (builder-tyyli).
    #[must_use]
    pub fn with_postconditions(mut self, clauses: Vec<Clause>) -> Self {
        self.postconditions = clauses;
        self
    }
}

/// Säieturvallinen rekisteri mainostetuista kyvyistä.
#[derive(Debug, Clone, Default)]
pub struct CapabilityRegistry {
    inner: Arc<RwLock<HashMap<MessageId, Capability>>>,
}

impl CapabilityRegistry {
    /// Luo tyhjän rekisterin.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Mainostaa (lisää tai korvaa) kyvyn ja palauttaa sen tunnisteen.
    pub async fn advertise(&self, capability: Capability) -> MessageId {
        let id = capability.id;
        let mut guard = self.inner.write().await;
        guard.insert(id, capability);
        id
    }

    /// Hakee kyvyn tunnisteen perusteella.
    pub async fn get(&self, id: MessageId) -> Option<Capability> {
        let guard = self.inner.read().await;
        guard.get(&id).cloned()
    }

    /// Palauttaa kaikki annetun nimiset kyvyt (tunnisteen mukaan järjestettynä).
    pub async fn find_by_name(&self, name: &str) -> Vec<Capability> {
        let guard = self.inner.read().await;
        let mut out: Vec<Capability> = guard
            .values()
            .filter(|c| c.name == name)
            .cloned()
            .collect();
        out.sort_by_key(|c| c.id);
        out
    }

    /// Rekisteröityjen kykyjen määrä.
    pub async fn len(&self) -> usize {
        let guard = self.inner.read().await;
        guard.len()
    }

    /// Onko rekisteri tyhjä.
    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.read().await;
        guard.is_empty()
    }
}

// ===========================================================================
// Sopimuksen tila ja toimite
// ===========================================================================

/// Sopimuksen tila (tilakone).
///
/// Sallitut siirtymät:
/// - `Proposed → Accepted`, `Proposed → Rejected`
/// - `Accepted → Fulfilled`, `Accepted → Failed`
///
/// `Rejected`, `Fulfilled` ja `Failed` ovat terminaalisia.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractStatus {
    /// Ehdotettu, odottaa hyväksyntää/hylkäystä.
    Proposed,
    /// Hyväksytty, työ menossa.
    Accepted,
    /// Hylätty ehdotusvaiheessa (terminaalinen).
    Rejected,
    /// Täytetty: toimite läpäisi tulosskeeman ja jälkiehdot (terminaalinen).
    Fulfilled,
    /// Epäonnistui: toimite rikkoi jälkiehdon tai tarjoaja ilmoitti virheen
    /// (terminaalinen).
    Failed,
}

impl ContractStatus {
    /// Onko tila terminaalinen.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ContractStatus::Rejected | ContractStatus::Fulfilled | ContractStatus::Failed
        )
    }

    /// Onko siirtymä `self → next` sallittu.
    #[must_use]
    pub fn can_transition_to(self, next: ContractStatus) -> bool {
        use ContractStatus::{Accepted, Failed, Fulfilled, Proposed, Rejected};
        matches!(
            (self, next),
            (Proposed, Accepted | Rejected) | (Accepted, Fulfilled | Failed)
        )
    }
}

/// Sopimuksen toimite (tarjoajan tuotos).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deliverable {
    /// Toimitteen tuottanut agentti.
    pub from: AgentId,

    /// Toimitteen hyötykuorma (tarkistetaan tulosskeemaa + jälkiehtoja vasten).
    pub payload: Value,

    /// Toimitushetki (UTC, injektoitu).
    pub at: Timestamp,
}

impl Deliverable {
    /// Rakentaa toimitteen.
    #[must_use]
    pub fn new(from: AgentId, payload: Value, at: Timestamp) -> Self {
        Self { from, payload, at }
    }
}

/// Yksittäinen sopimus pyytäjän ja tarjoajan välillä.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contract {
    /// Sopimuksen vakaa tunniste.
    pub id: MessageId,

    /// Sopimuksen pohjana oleva kyky (kopio mainostushetkeltä).
    pub capability: Capability,

    /// Työn pyytäjä.
    pub requester: AgentId,

    /// Työn tarjoaja.
    pub provider: AgentId,

    /// Sopimuksen syöte (validoitiin kyvyn syöteskeemaa vasten).
    pub input: Value,

    /// Tulosskeema jota toimitteen on noudatettava (kopio kyvystä).
    pub output_schema: Schema,

    /// Jälkiehdot jotka toimitteen on täytettävä (kopio kyvystä).
    pub postconditions: Vec<Clause>,

    /// Sopimuksen nykyinen tila.
    pub status: ContractStatus,

    /// Toimite, kun täytetty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliverable: Option<Deliverable>,

    /// Linkki orkesteroinnin tehtävään, jos sopimus syntyi työnkulusta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<TaskId>,

    /// Luontihetki (UTC, injektoitu).
    pub created_at: Timestamp,

    /// Viimeisimmän muutoksen hetki (UTC, injektoitu).
    pub updated_at: Timestamp,
}

impl Contract {
    /// Liittää sopimuksen orkesteroinnin tehtävään (builder-tyyli).
    #[must_use]
    pub fn with_link(mut self, task: TaskId) -> Self {
        self.link = Some(task);
        self
    }
}

// ===========================================================================
// Virheet
// ===========================================================================

/// Sopimustoiminnon virhe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// Syöte ei läpäissyt kyvyn syöteskeemaa.
    InputSchemaViolation(Vec<SchemaViolation>),

    /// Esiehto ei toteutunut hyväksyttäessä.
    PreconditionFailed(String),

    /// Toimite ei läpäissyt tulosskeemaa.
    OutputSchemaViolation(Vec<SchemaViolation>),

    /// Toimite rikkoi jälkiehdon.
    PostconditionBreach(String),

    /// Yritetty laiton tilasiirtymä.
    IllegalTransition {
        /// Lähtötila.
        from: ContractStatus,
        /// Yritetty kohdetila.
        to: ContractStatus,
    },

    /// Sopimusta/kykyä ei löytynyt.
    NotFound(String),

    /// Sopimus hylättiin annetulla syyllä.
    Rejected(String),
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContractError::InputSchemaViolation(v) => {
                write!(f, "input schema violation: {}", join_violations(v))
            }
            ContractError::PreconditionFailed(c) => write!(f, "precondition failed: {c}"),
            ContractError::OutputSchemaViolation(v) => {
                write!(f, "output schema violation: {}", join_violations(v))
            }
            ContractError::PostconditionBreach(c) => write!(f, "postcondition breach: {c}"),
            ContractError::IllegalTransition { from, to } => {
                write!(f, "illegal contract transition: {from:?} -> {to:?}")
            }
            ContractError::NotFound(what) => write!(f, "contract not found: {what}"),
            ContractError::Rejected(reason) => write!(f, "contract rejected: {reason}"),
        }
    }
}

impl std::error::Error for ContractError {}

/// Yhdistää rikkomukset luettavaksi merkkijonoksi.
fn join_violations(v: &[SchemaViolation]) -> String {
    v.iter()
        .map(|x| format!("{}: {}", x.field, x.reason))
        .collect::<Vec<_>>()
        .join("; ")
}

impl From<ContractError> for FamilyClawError {
    /// Muuntaa sopimusvirheen alustan keskitettyyn virhetyyppiin.
    ///
    /// `NotFound` kuvautuu [`FamilyClawError::NotFound`]:iin; kaikki muut
    /// (validointi-, ehto- ja siirtymävirheet) [`FamilyClawError::InvalidInput`]:iin,
    /// koska ne ovat syöte-/tilavirheitä.
    fn from(err: ContractError) -> Self {
        match err {
            ContractError::NotFound(what) => FamilyClawError::not_found(what),
            other => FamilyClawError::invalid_input(other.to_string()),
        }
    }
}

/// Sopimustoiminnon tulostyyppi.
pub type ContractResult<T> = std::result::Result<T, ContractError>;

// ===========================================================================
// Sopimustaulu
// ===========================================================================

/// Säieturvallinen sopimustaulu.
///
/// Hoitaa sopimusten elinkaaren: ehdota → hyväksy/hylkää → täytä/epäonnistu.
/// [`fulfill`](Self::fulfill) on **todentava** metodi: se ajaa tulosskeeman ja
/// jokaisen jälkiehdon toimitetta vasten, ja vain täysi läpäisy siirtää
/// sopimuksen tilaan [`ContractStatus::Fulfilled`].
#[derive(Debug, Clone, Default)]
pub struct ContractBoard {
    inner: Arc<RwLock<HashMap<MessageId, Contract>>>,
}

impl ContractBoard {
    /// Luo tyhjän taulun.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Ehdottaa sopimusta kyvylle. Validoi `input` kyvyn syöteskeemaa vasten.
    ///
    /// Onnistuessa luo sopimuksen tilaan [`ContractStatus::Proposed`].
    ///
    /// # Errors
    /// [`ContractError::InputSchemaViolation`] jos syöte ei läpäise
    /// kyvyn syöteskeemaa.
    pub async fn propose(
        &self,
        capability: &Capability,
        requester: AgentId,
        provider: AgentId,
        input: Value,
        now: Timestamp,
    ) -> ContractResult<Contract> {
        let violations = capability.input.check(&input);
        if !violations.is_empty() {
            return Err(ContractError::InputSchemaViolation(violations));
        }
        let contract = Contract {
            id: MessageId::new(),
            capability: capability.clone(),
            requester,
            provider,
            input,
            output_schema: capability.output.clone(),
            postconditions: capability.postconditions.clone(),
            status: ContractStatus::Proposed,
            deliverable: None,
            link: None,
            created_at: now,
            updated_at: now,
        };
        let mut guard = self.inner.write().await;
        guard.insert(contract.id, contract.clone());
        Ok(contract)
    }

    /// Lisää valmiiksi rakennetun sopimuksen tauluun (esim. linkitetty
    /// orkesterointiin). Ohittaa skeematarkistuksen — kutsujan vastuulla.
    ///
    /// # Errors
    /// [`ContractError::NotFound`] ei koskaan; mutta jos sama tunniste on jo
    /// taululla, vanha korvataan (idempotentti).
    pub async fn insert(&self, contract: Contract) {
        let mut guard = self.inner.write().await;
        guard.insert(contract.id, contract);
    }

    /// Hyväksyy ehdotetun sopimuksen. Tarkistaa esiehdot uudelleen syötettä
    /// vasten.
    ///
    /// # Errors
    /// - [`ContractError::NotFound`] jos sopimusta ei ole.
    /// - [`ContractError::IllegalTransition`] jos sopimus ei ole `Proposed`.
    /// - [`ContractError::PreconditionFailed`] jos jokin esiehto ei toteudu.
    pub async fn accept(&self, id: MessageId, now: Timestamp) -> ContractResult<Contract> {
        let mut guard = self.inner.write().await;
        let contract = guard
            .get_mut(&id)
            .ok_or_else(|| ContractError::NotFound(id.to_string()))?;

        if !contract.status.can_transition_to(ContractStatus::Accepted) {
            return Err(ContractError::IllegalTransition {
                from: contract.status,
                to: ContractStatus::Accepted,
            });
        }
        // Esiehdot syötettä vasten.
        for clause in &contract.capability.preconditions {
            if !clause.eval(&contract.input) {
                return Err(ContractError::PreconditionFailed(clause.describe()));
            }
        }
        contract.status = ContractStatus::Accepted;
        contract.updated_at = now;
        Ok(contract.clone())
    }

    /// Hylkää ehdotetun sopimuksen annetulla syyllä.
    ///
    /// # Errors
    /// - [`ContractError::NotFound`] jos sopimusta ei ole.
    /// - [`ContractError::IllegalTransition`] jos sopimus ei ole `Proposed`.
    pub async fn reject(
        &self,
        id: MessageId,
        reason: impl Into<String>,
        now: Timestamp,
    ) -> ContractResult<Contract> {
        let mut guard = self.inner.write().await;
        let contract = guard
            .get_mut(&id)
            .ok_or_else(|| ContractError::NotFound(id.to_string()))?;
        if !contract.status.can_transition_to(ContractStatus::Rejected) {
            return Err(ContractError::IllegalTransition {
                from: contract.status,
                to: ContractStatus::Rejected,
            });
        }
        contract.status = ContractStatus::Rejected;
        contract.updated_at = now;
        let _ = reason; // syy talletetaan tapahtumaan/lokiin, ei kenttään
        Ok(contract.clone())
    }

    /// **Todentava täyttö.** Ajaa toimitteen tulosskeeman ja jokaisen
    /// jälkiehdon läpi. Mikä tahansa rikkomus → `Accepted → Failed` ja
    /// kuvaava virhe. Täysi läpäisy → `Accepted → Fulfilled`.
    ///
    /// # Errors
    /// - [`ContractError::NotFound`] jos sopimusta ei ole.
    /// - [`ContractError::IllegalTransition`] jos sopimus ei ole `Accepted`.
    /// - [`ContractError::OutputSchemaViolation`] jos toimite rikkoo
    ///   tulosskeeman (sopimus siirtyy `Failed`-tilaan).
    /// - [`ContractError::PostconditionBreach`] jos jokin jälkiehto ei toteudu
    ///   (sopimus siirtyy `Failed`-tilaan).
    pub async fn fulfill(
        &self,
        id: MessageId,
        deliverable: Deliverable,
        now: Timestamp,
    ) -> ContractResult<Contract> {
        let mut guard = self.inner.write().await;
        let contract = guard
            .get_mut(&id)
            .ok_or_else(|| ContractError::NotFound(id.to_string()))?;

        if contract.status != ContractStatus::Accepted {
            return Err(ContractError::IllegalTransition {
                from: contract.status,
                to: ContractStatus::Fulfilled,
            });
        }

        // 1) Tulosskeema.
        let violations = contract.output_schema.check(&deliverable.payload);
        if !violations.is_empty() {
            contract.status = ContractStatus::Failed;
            contract.deliverable = Some(deliverable);
            contract.updated_at = now;
            return Err(ContractError::OutputSchemaViolation(violations));
        }

        // 2) Jokainen jälkiehto.
        for clause in &contract.postconditions {
            if !clause.eval(&deliverable.payload) {
                contract.status = ContractStatus::Failed;
                contract.deliverable = Some(deliverable.clone());
                contract.updated_at = now;
                return Err(ContractError::PostconditionBreach(clause.describe()));
            }
        }

        // Täysi läpäisy.
        contract.status = ContractStatus::Fulfilled;
        contract.deliverable = Some(deliverable);
        contract.updated_at = now;
        Ok(contract.clone())
    }

    /// Merkitsee hyväksytyn sopimuksen epäonnistuneeksi (tarjoaja ei pysty
    /// toimittamaan) annetulla syyllä.
    ///
    /// # Errors
    /// - [`ContractError::NotFound`] jos sopimusta ei ole.
    /// - [`ContractError::IllegalTransition`] jos sopimus ei ole `Accepted`.
    pub async fn fail(
        &self,
        id: MessageId,
        reason: impl Into<String>,
        now: Timestamp,
    ) -> ContractResult<Contract> {
        let mut guard = self.inner.write().await;
        let contract = guard
            .get_mut(&id)
            .ok_or_else(|| ContractError::NotFound(id.to_string()))?;
        if !contract.status.can_transition_to(ContractStatus::Failed) {
            return Err(ContractError::IllegalTransition {
                from: contract.status,
                to: ContractStatus::Failed,
            });
        }
        contract.status = ContractStatus::Failed;
        contract.updated_at = now;
        let _ = reason;
        Ok(contract.clone())
    }

    /// Hakee sopimuksen tunnisteen perusteella.
    pub async fn get(&self, id: MessageId) -> Option<Contract> {
        let guard = self.inner.read().await;
        guard.get(&id).cloned()
    }

    /// Listaa kaikki sopimukset (tunnisteen mukaan järjestettynä).
    pub async fn list(&self) -> Vec<Contract> {
        let guard = self.inner.read().await;
        let mut out: Vec<Contract> = guard.values().cloned().collect();
        out.sort_by_key(|c| c.id);
        out
    }

    /// Listaa tietyn tarjoajan sopimukset.
    pub async fn list_for_provider(&self, provider: AgentId) -> Vec<Contract> {
        let guard = self.inner.read().await;
        let mut out: Vec<Contract> = guard
            .values()
            .filter(|c| c.provider == provider)
            .cloned()
            .collect();
        out.sort_by_key(|c| c.id);
        out
    }

    /// Listaa tietyssä tilassa olevat sopimukset.
    pub async fn list_by_status(&self, status: ContractStatus) -> Vec<Contract> {
        let guard = self.inner.read().await;
        let mut out: Vec<Contract> = guard
            .values()
            .filter(|c| c.status == status)
            .cloned()
            .collect();
        out.sort_by_key(|c| c.id);
        out
    }

    /// Listaa tiettyyn orkesterointitehtävään linkitetyt sopimukset.
    pub async fn list_for_task(&self, task: TaskId) -> Vec<Contract> {
        let guard = self.inner.read().await;
        let mut out: Vec<Contract> = guard
            .values()
            .filter(|c| c.link == Some(task))
            .cloned()
            .collect();
        out.sort_by_key(|c| c.id);
        out
    }

    /// Sopimusten määrä taululla.
    pub async fn len(&self) -> usize {
        let guard = self.inner.read().await;
        guard.len()
    }

    /// Onko taulu tyhjä.
    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.read().await;
        guard.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time;
    use serde_json::json;

    fn ts(secs: i64) -> Timestamp {
        time::from_unix_secs(secs).expect("valid unix seconds")
    }

    fn render_capability() -> Capability {
        Capability::new(
            "render_video",
            Schema::new(vec![
                Field::required("script", FieldType::Str),
                Field::required("duration", FieldType::Int),
            ]),
            Schema::new(vec![
                Field::required("url", FieldType::Str),
                Field::required("frames", FieldType::Int),
            ]),
        )
        .with_preconditions(vec![Clause::gte("duration", json!(1))])
        .with_postconditions(vec![
            Clause::non_empty("url"),
            Clause::gte("frames", json!(1)),
        ])
    }

    // --- Schema.check ------------------------------------------------------

    #[test]
    fn schema_check_passes_valid_object() {
        let schema = Schema::new(vec![Field::required("a", FieldType::Str)]);
        assert!(schema.check(&json!({ "a": "x" })).is_empty());
    }

    #[test]
    fn schema_check_reports_missing_required() {
        let schema = Schema::new(vec![Field::required("a", FieldType::Str)]);
        let v = schema.check(&json!({}));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].field, "a");
    }

    #[test]
    fn schema_check_reports_wrong_type() {
        let schema = Schema::new(vec![Field::required("n", FieldType::Int)]);
        let v = schema.check(&json!({ "n": "not a number" }));
        assert_eq!(v.len(), 1);
        assert!(v[0].reason.contains("number"));
    }

    #[test]
    fn schema_check_optional_absent_ok_but_present_typechecked() {
        let schema = Schema::new(vec![Field::optional("o", FieldType::Bool)]);
        assert!(schema.check(&json!({})).is_empty());
        assert!(!schema.check(&json!({ "o": "x" })).is_empty());
        assert!(schema.check(&json!({ "o": true })).is_empty());
    }

    #[test]
    fn schema_check_non_object_is_root_violation() {
        let schema = Schema::empty();
        let v = schema.check(&json!("a string"));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].field, "$root");
    }

    // --- Clause truth table ------------------------------------------------

    #[test]
    fn clause_present_truth_table() {
        let c = Clause::present("x");
        assert!(c.eval(&json!({ "x": 1 })));
        assert!(!c.eval(&json!({ "x": null })));
        assert!(!c.eval(&json!({})));
    }

    #[test]
    fn clause_non_empty_truth_table() {
        let c = Clause::non_empty("x");
        assert!(c.eval(&json!({ "x": "a" })));
        assert!(c.eval(&json!({ "x": [1] })));
        assert!(c.eval(&json!({ "x": { "k": 1 } })));
        assert!(!c.eval(&json!({ "x": "" })));
        assert!(!c.eval(&json!({ "x": [] })));
        assert!(!c.eval(&json!({ "x": 5 }))); // numero ei ole "tyhjennettävä"
    }

    #[test]
    fn clause_eq_truth_table() {
        let c = Clause::eq("x", json!("ok"));
        assert!(c.eval(&json!({ "x": "ok" })));
        assert!(!c.eval(&json!({ "x": "no" })));
        assert!(!c.eval(&json!({})));
    }

    #[test]
    fn clause_gte_lte_truth_table() {
        let gte = Clause::gte("n", json!(10));
        assert!(gte.eval(&json!({ "n": 10 })));
        assert!(gte.eval(&json!({ "n": 11 })));
        assert!(!gte.eval(&json!({ "n": 9 })));
        assert!(!gte.eval(&json!({ "n": "x" })));

        let lte = Clause::lte("n", json!(10));
        assert!(lte.eval(&json!({ "n": 10 })));
        assert!(lte.eval(&json!({ "n": 9 })));
        assert!(!lte.eval(&json!({ "n": 11 })));
    }

    #[test]
    fn clause_min_max_len_truth_table() {
        let min = Clause::min_len("s", 3);
        assert!(min.eval(&json!({ "s": "abc" })));
        assert!(min.eval(&json!({ "s": [1, 2, 3, 4] })));
        assert!(!min.eval(&json!({ "s": "ab" })));

        let max = Clause::max_len("s", 3);
        assert!(max.eval(&json!({ "s": "abc" })));
        assert!(!max.eval(&json!({ "s": "abcd" })));
        assert!(!max.eval(&json!({ "s": 5 }))); // numerolla ei pituutta
    }

    #[test]
    fn clause_describe_is_readable() {
        assert_eq!(Clause::present("x").describe(), "x present");
        assert_eq!(Clause::min_len("y", 2).describe(), "len(y) >= 2");
    }

    // --- ContractStatus matrix ---------------------------------------------

    #[test]
    fn contract_status_transition_matrix() {
        use ContractStatus::{Accepted, Failed, Fulfilled, Proposed, Rejected};
        assert!(Proposed.can_transition_to(Accepted));
        assert!(Proposed.can_transition_to(Rejected));
        assert!(Accepted.can_transition_to(Fulfilled));
        assert!(Accepted.can_transition_to(Failed));

        // Laittomat.
        assert!(!Proposed.can_transition_to(Fulfilled));
        assert!(!Accepted.can_transition_to(Rejected));
        assert!(!Rejected.can_transition_to(Accepted));
        assert!(!Fulfilled.can_transition_to(Failed));
        assert!(!Failed.can_transition_to(Fulfilled));
    }

    #[test]
    fn contract_status_terminality() {
        assert!(ContractStatus::Rejected.is_terminal());
        assert!(ContractStatus::Fulfilled.is_terminal());
        assert!(ContractStatus::Failed.is_terminal());
        assert!(!ContractStatus::Proposed.is_terminal());
        assert!(!ContractStatus::Accepted.is_terminal());
    }

    // --- ContractBoard flow -------------------------------------------------

    #[tokio::test]
    async fn propose_rejects_bad_input_schema() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let err = board
            .propose(&cap, AgentId::new(), AgentId::new(), json!({ "script": "s" }), ts(1))
            .await
            .expect_err("missing duration");
        assert!(matches!(err, ContractError::InputSchemaViolation(_)));
    }

    #[tokio::test]
    async fn propose_accept_fulfill_happy_path() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let provider = AgentId::new();
        let c = board
            .propose(
                &cap,
                AgentId::new(),
                provider,
                json!({ "script": "hello", "duration": 5 }),
                ts(1),
            )
            .await
            .expect("propose");
        assert_eq!(c.status, ContractStatus::Proposed);

        let accepted = board.accept(c.id, ts(2)).await.expect("accept");
        assert_eq!(accepted.status, ContractStatus::Accepted);

        let deliverable = Deliverable::new(
            provider,
            json!({ "url": "https://x/v.mp4", "frames": 120 }),
            ts(3),
        );
        let fulfilled = board.fulfill(c.id, deliverable, ts(3)).await.expect("fulfill");
        assert_eq!(fulfilled.status, ContractStatus::Fulfilled);
        assert!(fulfilled.deliverable.is_some());
    }

    #[tokio::test]
    async fn fulfill_breaches_output_schema_sets_failed() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let provider = AgentId::new();
        let c = board
            .propose(
                &cap,
                AgentId::new(),
                provider,
                json!({ "script": "s", "duration": 2 }),
                ts(1),
            )
            .await
            .expect("propose");
        board.accept(c.id, ts(2)).await.expect("accept");

        // Toimite: "frames" puuttuu → tulosskeema rikkoutuu.
        let bad = Deliverable::new(provider, json!({ "url": "https://x" }), ts(3));
        let err = board.fulfill(c.id, bad, ts(3)).await.expect_err("schema breach");
        assert!(matches!(err, ContractError::OutputSchemaViolation(_)));

        let after = board.get(c.id).await.expect("present");
        assert_eq!(after.status, ContractStatus::Failed);
    }

    #[tokio::test]
    async fn fulfill_breaches_postcondition_sets_failed() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let provider = AgentId::new();
        let c = board
            .propose(
                &cap,
                AgentId::new(),
                provider,
                json!({ "script": "s", "duration": 2 }),
                ts(1),
            )
            .await
            .expect("propose");
        board.accept(c.id, ts(2)).await.expect("accept");

        // Skeema OK (url on merkkijono, frames on numero) mutta jälkiehto
        // `non_empty(url)` rikkoutuu (tyhjä) ja `frames >= 1` rikkoutuu (0).
        let bad = Deliverable::new(provider, json!({ "url": "", "frames": 0 }), ts(3));
        let err = board.fulfill(c.id, bad, ts(3)).await.expect_err("postcondition");
        assert!(matches!(err, ContractError::PostconditionBreach(_)));
        let after = board.get(c.id).await.expect("present");
        assert_eq!(after.status, ContractStatus::Failed);
    }

    #[tokio::test]
    async fn accept_rechecks_preconditions() {
        // Esiehto duration>=1 ei toteudu jos input ohitti skeematarkistuksen
        // toista reittiä. Tässä propose hyväksyy duration=0 (skeema vain vaatii
        // numeron), mutta accept torjuu esiehdon.
        let board = ContractBoard::new();
        let cap = render_capability();
        let c = board
            .propose(
                &cap,
                AgentId::new(),
                AgentId::new(),
                json!({ "script": "s", "duration": 0 }),
                ts(1),
            )
            .await
            .expect("propose");
        let err = board.accept(c.id, ts(2)).await.expect_err("precondition");
        assert!(matches!(err, ContractError::PreconditionFailed(_)));
    }

    #[tokio::test]
    async fn reject_only_from_proposed() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let c = board
            .propose(
                &cap,
                AgentId::new(),
                AgentId::new(),
                json!({ "script": "s", "duration": 2 }),
                ts(1),
            )
            .await
            .expect("propose");
        let rejected = board.reject(c.id, "too busy", ts(2)).await.expect("reject");
        assert_eq!(rejected.status, ContractStatus::Rejected);

        // Toinen reject → laiton siirtymä.
        let err = board.reject(c.id, "again", ts(3)).await.expect_err("terminal");
        assert!(matches!(err, ContractError::IllegalTransition { .. }));
    }

    #[tokio::test]
    async fn fulfill_requires_accepted() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let provider = AgentId::new();
        let c = board
            .propose(
                &cap,
                AgentId::new(),
                provider,
                json!({ "script": "s", "duration": 2 }),
                ts(1),
            )
            .await
            .expect("propose");
        // Yritä täyttää suoraan Proposed-tilasta → laiton.
        let d = Deliverable::new(provider, json!({ "url": "u", "frames": 1 }), ts(2));
        let err = board.fulfill(c.id, d, ts(2)).await.expect_err("not accepted");
        assert!(matches!(err, ContractError::IllegalTransition { .. }));
    }

    #[tokio::test]
    async fn queries_filter_correctly() {
        let board = ContractBoard::new();
        let cap = render_capability();
        let provider = AgentId::new();
        let other = AgentId::new();
        let task = TaskId::new();

        let c1 = board
            .propose(&cap, AgentId::new(), provider, json!({ "script": "a", "duration": 2 }), ts(1))
            .await
            .expect("c1");
        let mut linked = c1.clone();
        linked.link = Some(task);
        board.insert(linked).await;

        let _c2 = board
            .propose(&cap, AgentId::new(), other, json!({ "script": "b", "duration": 2 }), ts(1))
            .await
            .expect("c2");

        assert_eq!(board.len().await, 2);
        assert_eq!(board.list_for_provider(provider).await.len(), 1);
        assert_eq!(board.list_for_provider(other).await.len(), 1);
        assert_eq!(board.list_by_status(ContractStatus::Proposed).await.len(), 2);
        assert_eq!(board.list_for_task(task).await.len(), 1);
    }

    #[tokio::test]
    async fn capability_registry_advertise_and_find() {
        let reg = CapabilityRegistry::new();
        assert!(reg.is_empty().await);
        let cap = render_capability();
        let id = reg.advertise(cap.clone()).await;
        assert_eq!(reg.len().await, 1);
        assert_eq!(reg.get(id).await.map(|c| c.name), Some("render_video".into()));
        assert_eq!(reg.find_by_name("render_video").await.len(), 1);
        assert!(reg.find_by_name("nope").await.is_empty());
    }

    #[tokio::test]
    async fn contract_error_converts_to_familyclaw_error() {
        let nf: FamilyClawError = ContractError::NotFound("x".into()).into();
        assert!(matches!(nf, FamilyClawError::NotFound(_)));
        let bad: FamilyClawError =
            ContractError::PostconditionBreach("len(url) >= 1".into()).into();
        assert!(matches!(bad, FamilyClawError::InvalidInput(_)));
    }

    #[test]
    fn contract_serde_roundtrip() {
        let cap = render_capability();
        let c = Contract {
            id: MessageId::new(),
            capability: cap.clone(),
            requester: AgentId::new(),
            provider: AgentId::new(),
            input: json!({ "script": "s", "duration": 2 }),
            output_schema: cap.output.clone(),
            postconditions: cap.postconditions.clone(),
            status: ContractStatus::Proposed,
            deliverable: None,
            link: None,
            created_at: ts(1),
            updated_at: ts(1),
        };
        let json = serde_json::to_string(&c).expect("serialize");
        let back: Contract = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back);
    }
}
