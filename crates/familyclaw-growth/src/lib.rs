//! Kasvusilmukan **ehdotuspino** (Phase 4.5, roadmap §6.5) — KERROS A, OSS.
//!
//! Kasvusilmukan putki on `proof bundle → safe memory → pattern proposal →
//! eval proposal → approval-gated skill/policy update`. Tämä crate toteuttaa
//! sen **turvallisen ytimen**: [`Proposal`]-tietorakenteen ja
//! [`ProposalStore`]:n joka **kirjaa** ehdotuksia ja merkitsee niiden tilan
//! (hyväksytty/evätty) — **muttei KOSKAAN sovella niitä**.
//!
//! ## Kovat invariantit (roadmap §6.5, ei-neuvoteltavat)
//! - ❌ **Ei hiljaista itse-muokkausta.** Tämä crate ei sisällä `apply`-metodia
//!   eikä mitään polkua joka muuttaisi taitoa, käytäntöä tai oikeutta. Ehdotus
//!   on **inertti data**: se voi olla `Pending`/`Approved`/`Denied`, mutta sen
//!   *soveltaminen* (jos ja kun se rakennetaan) on erillinen, ihmisen
//!   hyväksyntäportin takana oleva askel toisessa PR:ssä — eikä se voi koskaan
//!   nostaa oikeuksia hiljaa.
//! - ❌ **Ei hiljaista oikeuksien laajennusta.** [`ProposalKind`] on
//!   tarkoituksella **kuvaileva** (ihmisluettava ehdotus + eval-kriteeri), ei
//!   suoritettava muutos. Mitä ehdotus *saa* lopulta muuttaa, on oma
//!   suunnittelupäätöksensä (ihminen päättää) ja toteutetaan vasta hyväksyntä-
//!   portin kanssa.
//! - ✅ Jokainen ehdotus kantaa **todiste-lähteensä** ([`Proposal::proof_sources`])
//!   ja **eval-kriteerinsä** ([`Proposal::eval`]) — ei muutosta ilman testiä
//!   joka todistaa hyödyn (peilaa Phase-3 recall-benchmark-kuria).
//! - ✅ **Hyväksyntä sitoutuu sisältöön, ei vain tunnisteeseen.** Päätös
//!   ([`ProposalStore::approve`] / [`ProposalStore::deny`]) vaatii ehdotuksen
//!   [sisältöhajautteen](Proposal::content_hash): jos pinossa oleva ehdotus on
//!   ehtinyt muuttua sen jälkeen kun ihminen katselmoi sen (TOCTOU-drift),
//!   päätös **epäonnistuu** ([`GrowthError::HashMismatch`]) — deny-by-default.
//! - ✅ **Pysyvä päätösjälki.** Jokainen päätös tuottaa [`ApprovalRecord`]:n
//!   joka jää pinoon kyseltäväksi ([`ProposalStore::approval_history`]) —
//!   auditointiketju ei katoa status-lipun alle.
//!
//! ## Apply-polun esivaatimukset (tilannekuva)
//!
//! `apply()`-metodia **EI ole olemassa eikä saa vielä rakentaa**. Ennen kuin
//! sellaista edes harkitaan (erillinen PR, ihmisen hyväksyntäportti), näiden
//! esivaatimusten on oltava paikallaan:
//!
//! - [x] **Sisältöhajautteeseen sidottu hyväksyntä** — TEHTY tässä cratessa:
//!   päätös sitoutuu ehdotuksen tarkkaan sisältöön, ei vain tunnisteeseen.
//! - [ ] TODO: **Polkujen kanonisointi + kieltolista** — mihin kohteisiin
//!   sovellus saisi ylipäätään koskea, normalisoituna ja kiellot edellä.
//! - [ ] TODO: **Pakollinen dry-run-diffi** — sovelluksen vaikutus on
//!   näytettävä ihmiselle diffina ennen suoritusta, ei jälkikäteen.
//! - [ ] TODO: **Palautussuunnitelma (revert plan)** — jokaisella
//!   sovelluksella on oltava todistettu tie takaisin ennen ensimmäistäkään
//!   suoritusta.
//!
//! Tämän craten **turvallisuus on rakenteellista**: koska `apply`-polkua ei ole
//! olemassa, hyväksymätön (tai hyväksyttykään) ehdotus ei voi muuttaa mitään
//! tämän craten kautta. Yksikkötesti `store_has_no_apply_path_only_records...`
//! dokumentoi tämän takuun.

use std::collections::HashMap;

use familyclaw_core::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub mod evidence;

pub use evidence::{
    evaluate_for_approval, EvidenceLedger, EvidenceVerdict, ImprovementMetric, ReplayEvidence,
};

/// Ehdotuksen yksilöivä tunniste.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProposalId(Uuid);

impl ProposalId {
    /// Luo uuden satunnaisen tunnisteen.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Rakentaa tunnisteen annetusta UUID:sta (vakaa, testeille).
    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Taustalla oleva UUID.
    #[must_use]
    pub const fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for ProposalId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ProposalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Päättäjän identiteetti — **geneerinen rooli-id** (KERROS A, OSS).
///
/// Tämä on tarkoituksella pelkkä läpinäkyvä newtype: se kantaa roolin
/// (esim. `"operator"`, `"reviewer-2"`), **ei koskaan** todellista
/// henkilöllisyyttä. Todellisten identiteettien sidonta (jos sellaista
/// tarvitaan) kuuluu yksityiseen kerrokseen, ei tähän OSS-crateen.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApproverId(String);

impl ApproverId {
    /// Rakentaa päättäjä-tunnisteen geneerisestä rooli-id:stä.
    #[must_use]
    pub fn new(role: impl Into<String>) -> Self {
        Self(role.into())
    }

    /// Rooli-id merkkijonona.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ApproverId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Ihmisen päätös ehdotuksesta. **Päätös ei sovella mitään** — se on
/// kirjattu tahdonilmaus, jonka mahdollinen toimeenpano on erillinen,
/// portin takana oleva askel (jota tämä crate ei tee).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    /// Ehdotus hyväksyttiin (EI vielä sovellettu).
    Approved,
    /// Ehdotus evättiin.
    Denied {
        /// Ihmisluettava perustelu epäykselle (auditointijälkeä varten).
        reason: String,
    },
}

/// Pysyvä päätöskirjaus: kuka päätti, mistä tarkasta sisällöstä ja milloin.
///
/// `content_hash` sitoo päätöksen ehdotuksen **tarkkaan sisältöön**
/// päätöshetkellä ([`Proposal::content_hash`]) — jos ehdotus myöhemmin
/// muuttuisi, kirjaus todistaa mihin versioon päätös kohdistui.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    /// Päätöksen kohteena ollut ehdotus.
    pub proposal_id: ProposalId,
    /// Ehdotuksen sisältöhajaute päätöshetkellä (SHA-256).
    pub content_hash: [u8; 32],
    /// Päättäjän geneerinen rooli-id.
    pub approver: ApproverId,
    /// Päätöshetki (injektoitu kello).
    pub decided_at: Timestamp,
    /// Tehty päätös.
    pub decision: Decision,
}

/// Kasvusilmukan virhetyypit. Epäonnistuminen on **äänekäs** (`Err`), ei
/// hiljainen `false` — deny-by-default: epävarma päätös ei mene läpi.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GrowthError {
    /// Ehdotuksen sisältö pinossa ei vastaa hajautetta jonka päättäjä
    /// katselmoi → päätös evätään (TOCTOU-drift-suoja).
    #[error(
        "content hash mismatch for proposal {id}: the stored proposal no longer matches what \
         was reviewed — decision refused (deny-by-default)"
    )]
    HashMismatch {
        /// Ehdotus jonka sisältö ei täsmännyt.
        id: ProposalId,
    },
    /// Ehdotusta ei löytynyt pinosta.
    #[error("proposal not found: {id}")]
    ProposalNotFound {
        /// Tuntematon tunniste.
        id: ProposalId,
    },
    /// Ehdotus on jo päätetty — päätöstä ei voi tehdä (eikä ylikirjoittaa)
    /// uudelleen tätä kautta.
    #[error("proposal {id} is already decided ({status:?}); decisions are not overwritable")]
    AlreadyDecided {
        /// Jo päätetty ehdotus.
        id: ProposalId,
        /// Ehdotuksen nykyinen (päätetty) tila.
        status: ProposalStatus,
    },
}

/// Mitä ehdotus *koskee* — **kuvaileva**, ei suoritettava (kova invariantti:
/// ei hiljaista muutosta). Jokainen variantti on ihmisluettava pyyntö, ei
/// koneellinen mutaatio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalKind {
    /// Havaittu toistuva kuvio joka ehdottaa uutta tai muokattua taitoa.
    SkillPattern {
        /// Ihmisluettava kuvaus kuviosta (ei koodia, ei manifestidiffiä).
        summary: String,
    },
    /// Havaittu käytäntö joka esti turvallisen tapauksen toistuvasti.
    PolicyFriction {
        /// Ihmisluettava kuvaus mitä estyi ja miksi se vaikuttaa väärältä.
        summary: String,
    },
}

/// Ehdotuksen elinkaaren tila. **Soveltaminen ei tapahdu tässä cratessa** —
/// `Approved` tarkoittaa vain että ihminen on hyväksynyt; mahdollinen
/// soveltaminen on erillinen, portin takana oleva askel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus {
    /// Odottaa ihmisen päätöstä (oletus uudelle ehdotukselle).
    Pending,
    /// Ihminen hyväksyi ehdotuksen (EI vielä sovellettu — soveltaminen on
    /// erillinen askel jota tämä crate ei tee).
    Approved,
    /// Ihminen hylkäsi ehdotuksen.
    Denied,
}

/// Eval-kriteeri: miten ehdotuksen hyöty *todistettaisiin* ennen soveltamista
/// (peilaa Phase-3 recall-benchmark-kuria: ei muutosta ilman testiä).
/// Kuvaileva — varsinaista evalia ei aja tämä crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalCriteria {
    /// Ihmisluettava kuvaus miten hyöty mitattaisiin (esim. "recall@5 paranee
    /// fixturella X ilman regressiota Y:ssä").
    pub description: String,
}

/// Yksittäinen kasvuehdotus — **inertti data**, ei suoritettava muutos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    /// Yksilöivä tunniste.
    pub id: ProposalId,
    /// Mitä ehdotus koskee (kuvaileva).
    pub kind: ProposalKind,
    /// Eval-kriteeri: miten hyöty todistettaisiin ennen soveltamista.
    pub eval: EvalCriteria,
    /// Todiste-lähteet (proof-bundle-tunnisteet merkkijonoina) jotka
    /// motivoivat ehdotuksen — ketju auditoitavaksi.
    pub proof_sources: Vec<String>,
    /// Elinkaaren tila.
    pub status: ProposalStatus,
    /// Luontihetki (injektoitu kello).
    pub created_at: Timestamp,
}

/// Domain-separaatio + versiotagi sisältöhajautteelle. Versio nostetaan jos
/// kanoninen muoto joskus muuttuu — vanhat hajautteet eivät silloin täsmää
/// vahingossa (deny-by-default).
const CONTENT_HASH_DOMAIN: &[u8] = b"familyclaw-growth/proposal-content/v1\n";

/// Kanoninen **sisältönäkymä** ehdotuksesta hajautusta varten: sama kuin
/// [`Proposal`] mutta **ilman muuttuvaa `status`-kenttää**. Kenttäjärjestys on
/// kiinteä (deklaraatiojärjestys), joten `serde_json`-sarjallistus on
/// deterministinen samalle arvolle.
#[derive(Serialize)]
struct ProposalContentView<'a> {
    id: &'a ProposalId,
    kind: &'a ProposalKind,
    eval: &'a EvalCriteria,
    proof_sources: &'a [String],
    created_at: &'a Timestamp,
}

impl Proposal {
    /// Rakentaa uuden `Pending`-ehdotuksen.
    #[must_use]
    pub fn new(
        kind: ProposalKind,
        eval: EvalCriteria,
        proof_sources: Vec<String>,
        created_at: Timestamp,
    ) -> Self {
        Self {
            id: ProposalId::new(),
            kind,
            eval,
            proof_sources,
            status: ProposalStatus::Pending,
            created_at,
        }
    }

    /// Ehdotuksen **sisältöhajaute** (SHA-256): kanoninen sarjallistus
    /// kaikista kentistä **paitsi muuttuvasta `status`-kentästä**.
    ///
    /// Hyväksyntä sidotaan tähän hajautteeseen tunnisteen sijaan, jotta
    /// `record → (ihminen katselmoi) → approve` -polussa ehdotuksen sisältö
    /// ei voi vaihtua huomaamatta katselmoinnin ja päätöksen välissä
    /// (TOCTOU). Status-kentän muuttaminen EI muuta hajautetta — päätös
    /// koskee sisältöä, ei elinkaaritilaa.
    #[must_use]
    pub fn content_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(CONTENT_HASH_DOMAIN);
        let view = ProposalContentView {
            id: &self.id,
            kind: &self.kind,
            eval: &self.eval,
            proof_sources: &self.proof_sources,
            created_at: &self.created_at,
        };
        if let Ok(bytes) = serde_json::to_vec(&view) {
            hasher.update(&bytes);
        } else {
            // Käytännössä saavuttamaton: näkymä on pelkkää dataa
            // (merkkijonot, UUID, UTC-aikaleima) jonka JSON-sarjallistus
            // ei epäonnistu. Jos näin silti kävisi, EI paniikkia eikä
            // hiljaista nollahajautetta — syötetään merkkiliite jota
            // onnistunut sarjallistus (alkaa aina `{`:lla) ei voi
            // tuottaa, sidottuna tunnisteeseen. Tuloksena hajaute joka
            // ei täsmää mihinkään katselmoituun sisältöön →
            // deny-by-default.
            hasher.update(b"!content-serialization-failure:");
            hasher.update(self.id.as_uuid().as_bytes());
        }
        hasher.finalize().into()
    }
}

/// Kasvusilmukan ehdotuspino: **kirjaa** ehdotuksia, merkitsee niiden tilan ja
/// säilyttää pysyvän päätösjäljen ([`ApprovalRecord`]).
///
/// **Tarkoituksellinen rajaus (kova invariantti):** tällä tyypillä EI ole
/// `apply`-metodia eikä mitään tapaa muuttaa taitoa/käytäntöä/oikeutta. Se on
/// puhtaasti kirjaava + tila-merkkaava. Ehdotuksen soveltaminen (jos ja kun se
/// rakennetaan) on erillinen, ihmisen hyväksyntäportin takana oleva askel.
///
/// Päätökset ([`approve`](Self::approve) / [`deny`](Self::deny)) vaativat
/// katselmoidun sisällön hajautteen ja epäonnistuvat äänekkäästi
/// ([`GrowthError`]) jos sisältö on driftannut, ehdotusta ei ole tai se on
/// jo päätetty.
#[derive(Debug, Default)]
pub struct ProposalStore {
    proposals: HashMap<ProposalId, Proposal>,
    approvals: Vec<ApprovalRecord>,
}

impl ProposalStore {
    /// Luo tyhjän pinon.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Kirjaa ehdotuksen (tila aina `Pending` riippumatta annetusta). Palauttaa
    /// tunnisteen. EI sovella mitään.
    ///
    /// Huom: jos samalla tunnisteella kirjataan uudelleen, aiempi sisältö
    /// korvautuu ja tila palaa `Pending`iksi — mutta mikään aiemman sisällön
    /// pohjalta katselmoitu hajaute ei enää täsmää uuteen sisältöön, joten
    /// vanhentunut hyväksyntä-yritys kaatuu [`GrowthError::HashMismatch`]iin
    /// (deny-by-default). Jo tehdyt [`ApprovalRecord`]-kirjaukset säilyvät.
    pub fn record(&mut self, mut proposal: Proposal) -> ProposalId {
        proposal.status = ProposalStatus::Pending;
        let id = proposal.id;
        self.proposals.insert(id, proposal);
        id
    }

    /// Hakee ehdotuksen tunnisteella.
    #[must_use]
    pub fn get(&self, id: ProposalId) -> Option<&Proposal> {
        self.proposals.get(&id)
    }

    /// Kaikki ehdotukset (introspektio operaattoripinnalle).
    #[must_use]
    pub fn all(&self) -> Vec<&Proposal> {
        self.proposals.values().collect()
    }

    /// Vain odottavat ehdotukset (ihmisen päätettäväksi).
    #[must_use]
    pub fn pending(&self) -> Vec<&Proposal> {
        self.proposals
            .values()
            .filter(|p| p.status == ProposalStatus::Pending)
            .collect()
    }

    /// Merkitsee ehdotuksen ihmisen hyväksymäksi ja kirjaa pysyvän
    /// [`ApprovalRecord`]:n.
    ///
    /// `expected_hash` on hajaute jonka päättäjä laski **katselmoimastaan**
    /// sisällöstä ([`Proposal::content_hash`]). Jos pinossa olevan ehdotuksen
    /// nykyinen sisältöhajaute ei täsmää, päätös **epäonnistuu**
    /// ([`GrowthError::HashMismatch`]) — hyväksyntä sitoutuu tarkkaan
    /// sisältöön, ei tunnisteeseen (TOCTOU-suoja, deny-by-default).
    ///
    /// **Tämä EI sovella ehdotusta** — se vain kirjaa ihmisen päätöksen. Mikään
    /// taito/käytäntö/oikeus ei muutu tämän kutsun seurauksena.
    pub fn approve(
        &mut self,
        id: ProposalId,
        expected_hash: [u8; 32],
        approver: ApproverId,
        now: Timestamp,
    ) -> Result<ApprovalRecord, GrowthError> {
        self.decide(id, expected_hash, approver, now, Decision::Approved)
    }

    /// Merkitsee ehdotuksen ihmisen hylkäämäksi ja kirjaa pysyvän
    /// [`ApprovalRecord`]:n perusteluineen.
    ///
    /// Sama sisältöhajaute-portti kuin [`approve`](Self::approve): myös epäys
    /// sitoutuu tarkkaan katselmoituun sisältöön, jotta auditointijälki
    /// todistaa mistä versiosta päätös tehtiin.
    pub fn deny(
        &mut self,
        id: ProposalId,
        expected_hash: [u8; 32],
        approver: ApproverId,
        reason: impl Into<String>,
        now: Timestamp,
    ) -> Result<ApprovalRecord, GrowthError> {
        self.decide(
            id,
            expected_hash,
            approver,
            now,
            Decision::Denied {
                reason: reason.into(),
            },
        )
    }

    /// Päätöshistoria annetulle ehdotukselle (kirjausjärjestyksessä).
    #[must_use]
    pub fn approval_history(&self, id: ProposalId) -> Vec<&ApprovalRecord> {
        self.approvals
            .iter()
            .filter(|r| r.proposal_id == id)
            .collect()
    }

    /// Koko päätösjälki (kirjausjärjestyksessä, kaikki ehdotukset).
    #[must_use]
    pub fn approvals(&self) -> &[ApprovalRecord] {
        &self.approvals
    }

    /// Yhteinen päätöspolku: löytyminen → ei jo päätetty → sisältöhajaute
    /// täsmää → status-merkintä + pysyvä kirjaus. Epäonnistuminen missä
    /// tahansa portissa on `Err` eikä muuta mitään.
    fn decide(
        &mut self,
        id: ProposalId,
        expected_hash: [u8; 32],
        approver: ApproverId,
        decided_at: Timestamp,
        decision: Decision,
    ) -> Result<ApprovalRecord, GrowthError> {
        let proposal = self
            .proposals
            .get_mut(&id)
            .ok_or(GrowthError::ProposalNotFound { id })?;
        if proposal.status != ProposalStatus::Pending {
            return Err(GrowthError::AlreadyDecided {
                id,
                status: proposal.status,
            });
        }
        let actual = proposal.content_hash();
        if actual != expected_hash {
            return Err(GrowthError::HashMismatch { id });
        }
        proposal.status = match decision {
            Decision::Approved => ProposalStatus::Approved,
            Decision::Denied { .. } => ProposalStatus::Denied,
        };
        let record = ApprovalRecord {
            proposal_id: id,
            content_hash: actual,
            approver,
            decided_at,
            decision,
        };
        self.approvals.push(record.clone());
        Ok(record)
    }

    /// Ehdotusten lukumäärä.
    #[must_use]
    pub fn len(&self) -> usize {
        self.proposals.len()
    }

    /// Onko pino tyhjä.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.proposals.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> Timestamp {
        familyclaw_core::time::from_unix_secs(secs).expect("valid unix seconds")
    }

    fn sample() -> Proposal {
        Proposal::new(
            ProposalKind::SkillPattern {
                summary: "skill fs_read often needs a recursive flag".to_string(),
            },
            EvalCriteria {
                description: "prove recursive read passes a fixture without widening allowlist"
                    .to_string(),
            },
            vec!["proof-1".to_string(), "proof-2".to_string()],
            at(1000),
        )
    }

    fn operator() -> ApproverId {
        ApproverId::new("operator")
    }

    #[test]
    fn new_proposal_is_pending() {
        assert_eq!(sample().status, ProposalStatus::Pending);
    }

    #[test]
    fn record_forces_pending_and_returns_id() {
        let mut store = ProposalStore::new();
        let mut p = sample();
        p.status = ProposalStatus::Approved; // yritä huijata sisään hyväksyttynä
        let id = store.record(p);
        assert_eq!(
            store.get(id).expect("present").status,
            ProposalStatus::Pending,
            "record pakottaa Pendingiksi — ei voi kirjata valmiiksi hyväksyttyä"
        );
    }

    #[test]
    fn content_hash_is_stable_across_status_change() {
        let mut p = sample();
        let before = p.content_hash();
        p.status = ProposalStatus::Approved;
        let after_approved = p.content_hash();
        p.status = ProposalStatus::Denied;
        let after_denied = p.content_hash();
        assert_eq!(
            before, after_approved,
            "status EI kuulu sisältöhajautteeseen"
        );
        assert_eq!(before, after_denied, "status EI kuulu sisältöhajautteeseen");
    }

    #[test]
    fn content_hash_changes_when_content_changes() {
        let p = sample();
        let original = p.content_hash();

        let mut tampered = p.clone();
        tampered.kind = ProposalKind::SkillPattern {
            summary: "grant unrestricted filesystem access".to_string(),
        };
        assert_ne!(
            original,
            tampered.content_hash(),
            "sisällön muutos muuttaa hajautteen"
        );

        let mut extra_proof = p.clone();
        extra_proof.proof_sources.push("proof-3".to_string());
        assert_ne!(
            original,
            extra_proof.content_hash(),
            "todiste-lähteiden muutos muuttaa hajautteen"
        );
    }

    #[test]
    fn approve_with_correct_hash_succeeds_and_records_history() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        let hash = store.get(id).expect("present").content_hash();

        let record = store
            .approve(id, hash, operator(), at(2000))
            .expect("approve with the reviewed hash succeeds");

        assert_eq!(
            store.get(id).expect("present").status,
            ProposalStatus::Approved
        );
        assert_eq!(record.proposal_id, id);
        assert_eq!(record.content_hash, hash);
        assert_eq!(record.approver, operator());
        assert_eq!(record.decided_at, at(2000));
        assert_eq!(record.decision, Decision::Approved);

        let history = store.approval_history(id);
        assert_eq!(history.len(), 1, "päätös jättää pysyvän kirjauksen");
        assert_eq!(history[0], &record);
    }

    #[test]
    fn approve_with_wrong_hash_returns_hash_mismatch() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        let wrong = [0u8; 32];

        let err = store
            .approve(id, wrong, operator(), at(2000))
            .expect_err("drifted content must not be approvable");
        assert_eq!(err, GrowthError::HashMismatch { id });

        // Deny-by-default: mikään ei muuttunut, mitään ei kirjattu.
        assert_eq!(
            store.get(id).expect("present").status,
            ProposalStatus::Pending
        );
        assert!(store.approval_history(id).is_empty());
    }

    #[test]
    fn approve_unknown_id_returns_proposal_not_found() {
        let mut store = ProposalStore::new();
        let unknown = ProposalId::new();
        let err = store
            .approve(unknown, [0u8; 32], operator(), at(2000))
            .expect_err("unknown id must be an error, not a silent false");
        assert_eq!(err, GrowthError::ProposalNotFound { id: unknown });
    }

    #[test]
    fn deny_records_a_denied_record() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        let hash = store.get(id).expect("present").content_hash();

        let record = store
            .deny(id, hash, operator(), "eval criteria too vague", at(3000))
            .expect("deny with the reviewed hash succeeds");

        assert_eq!(
            store.get(id).expect("present").status,
            ProposalStatus::Denied
        );
        assert_eq!(
            record.decision,
            Decision::Denied {
                reason: "eval criteria too vague".to_string()
            }
        );
        let history = store.approval_history(id);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].decision, record.decision);
    }

    #[test]
    fn already_decided_proposal_cannot_be_redecided() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        let hash = store.get(id).expect("present").content_hash();
        store
            .approve(id, hash, operator(), at(2000))
            .expect("first decision succeeds");

        let err = store
            .deny(id, hash, operator(), "changed my mind", at(2001))
            .expect_err("a decided proposal is immutable through this path");
        assert_eq!(
            err,
            GrowthError::AlreadyDecided {
                id,
                status: ProposalStatus::Approved
            }
        );
        assert_eq!(
            store.approval_history(id).len(),
            1,
            "epäonnistunut uudelleenpäätös ei lisää kirjauksia"
        );
    }

    /// TOCTOU-skenaario: ehdotus katselmoidaan, sen jälkeen sama tunniste
    /// ylikirjoitetaan eri sisällöllä — vanhentuneen katselmoinnin hajaute
    /// EI saa hyväksyä uutta sisältöä.
    #[test]
    fn rerecord_with_same_id_invalidates_stale_reviewed_hash() {
        let mut store = ProposalStore::new();
        let original = sample();
        let id = store.record(original.clone());
        let reviewed_hash = store.get(id).expect("present").content_hash();

        // Sisältö vaihtuu katselmoinnin ja päätöksen välissä (sama id).
        let mut swapped = sample();
        swapped.id = id;
        swapped.kind = ProposalKind::PolicyFriction {
            summary: "loosen the sandbox write policy".to_string(),
        };
        store.record(swapped);

        let err = store
            .approve(id, reviewed_hash, operator(), at(2000))
            .expect_err("stale hash must not approve swapped content");
        assert_eq!(err, GrowthError::HashMismatch { id });
        assert_eq!(
            store.get(id).expect("present").status,
            ProposalStatus::Pending,
            "vaihdettu sisältö jää odottamaan aitoa katselmointia"
        );
    }

    #[test]
    fn pending_filters_decided() {
        let mut store = ProposalStore::new();
        let a = store.record(sample());
        let _b = store.record(sample());
        let hash = store.get(a).expect("present").content_hash();
        store
            .approve(a, hash, operator(), at(2000))
            .expect("approve succeeds");
        assert_eq!(store.pending().len(), 1, "vain päättämättömät listataan");
        assert_eq!(store.all().len(), 2);
    }

    /// KOVA INVARIANTTI: pinolla EI OLE apply-polkua. Tämä testi dokumentoi
    /// rakenteellisen takuun — proposalin elinkaari on data-only, ja ainoat
    /// mutaatiot ovat status-merkinnät + pysyvät päätöskirjaukset. Mikään
    /// julkinen metodi ei muuta taitoa, käytäntöä tai oikeutta. (Jos joku
    /// lisää `apply`-metodin, tämä kommentti + PR-katselmointi on portti.)
    #[test]
    fn store_has_no_apply_path_only_records_and_marks_status() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        let hash = store.get(id).expect("present").content_hash();
        // Ainoat tila-mutaatiot: approve/deny. Hyväksyntä EI sovella → status
        // on Approved mutta mitään ulkoista ei muutu (tämä crate ei kosketa
        // mitään taito-/käytäntö-/oikeuspintaa).
        store
            .approve(id, hash, operator(), at(2000))
            .expect("approve succeeds");
        let p = store.get(id).expect("present");
        assert_eq!(p.status, ProposalStatus::Approved);
        // proof_sources + eval säilyvät muuttumattomina (auditoitavuus).
        assert_eq!(p.proof_sources, vec!["proof-1", "proof-2"]);
        assert!(!p.eval.description.is_empty());
    }

    #[test]
    fn proposal_roundtrips_json() {
        let p = sample();
        let json = serde_json::to_string(&p).expect("serialize");
        let back: Proposal = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
        assert_eq!(
            p.content_hash(),
            back.content_hash(),
            "hajaute säilyy sarjallistuskierroksen yli"
        );
    }

    #[test]
    fn approval_record_roundtrips_json() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        let hash = store.get(id).expect("present").content_hash();
        let record = store
            .deny(id, hash, operator(), "needs a sharper eval", at(4000))
            .expect("deny succeeds");
        let json = serde_json::to_string(&record).expect("serialize");
        let back: ApprovalRecord = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(record, back);
    }

    #[test]
    fn growth_error_messages_are_descriptive() {
        let id = ProposalId::new();
        assert!(GrowthError::HashMismatch { id }
            .to_string()
            .contains("deny-by-default"));
        assert!(GrowthError::ProposalNotFound { id }
            .to_string()
            .contains("not found"));
        assert!(GrowthError::AlreadyDecided {
            id,
            status: ProposalStatus::Denied
        }
        .to_string()
        .contains("already decided"));
    }
}
