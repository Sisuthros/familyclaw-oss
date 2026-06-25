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
//!
//! Tämän craten **turvallisuus on rakenteellista**: koska `apply`-polkua ei ole
//! olemassa, hyväksymätön (tai hyväksyttykään) ehdotus ei voi muuttaa mitään
//! tämän craten kautta. Yksikkötesti `store_has_no_apply_path_only_records...`
//! dokumentoi tämän takuun.

use std::collections::HashMap;

use familyclaw_core::Timestamp;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
}

/// Kasvusilmukan ehdotuspino: **kirjaa** ehdotuksia ja merkitsee niiden tilan.
///
/// **Tarkoituksellinen rajaus (kova invariantti):** tällä tyypillä EI ole
/// `apply`-metodia eikä mitään tapaa muuttaa taitoa/käytäntöä/oikeutta. Se on
/// puhtaasti kirjaava + tila-merkkaava. Ehdotuksen soveltaminen (jos ja kun se
/// rakennetaan) on erillinen, ihmisen hyväksyntäportin takana oleva askel.
#[derive(Debug, Default)]
pub struct ProposalStore {
    proposals: HashMap<ProposalId, Proposal>,
}

impl ProposalStore {
    /// Luo tyhjän pinon.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Kirjaa ehdotuksen (tila aina `Pending` riippumatta annetusta). Palauttaa
    /// tunnisteen. EI sovella mitään.
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

    /// Merkitsee ehdotuksen ihmisen hyväksymäksi. Palauttaa `true` jos löytyi.
    ///
    /// **Tämä EI sovella ehdotusta** — se vain kirjaa ihmisen päätöksen. Mikään
    /// taito/käytäntö/oikeus ei muutu tämän kutsun seurauksena.
    pub fn approve(&mut self, id: ProposalId) -> bool {
        self.set_status(id, ProposalStatus::Approved)
    }

    /// Merkitsee ehdotuksen ihmisen hylkäämäksi. Palauttaa `true` jos löytyi.
    pub fn deny(&mut self, id: ProposalId) -> bool {
        self.set_status(id, ProposalStatus::Denied)
    }

    fn set_status(&mut self, id: ProposalId, status: ProposalStatus) -> bool {
        if let Some(p) = self.proposals.get_mut(&id) {
            p.status = status;
            true
        } else {
            false
        }
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
    fn approve_and_deny_only_change_status() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        assert!(store.approve(id));
        assert_eq!(store.get(id).unwrap().status, ProposalStatus::Approved);
        assert!(store.deny(id));
        assert_eq!(store.get(id).unwrap().status, ProposalStatus::Denied);
        // Tuntematon → false, ei paniikkia.
        assert!(!store.approve(ProposalId::new()));
    }

    #[test]
    fn pending_filters_decided() {
        let mut store = ProposalStore::new();
        let a = store.record(sample());
        let _b = store.record(sample());
        store.approve(a);
        assert_eq!(store.pending().len(), 1, "vain päättämättömät listataan");
        assert_eq!(store.all().len(), 2);
    }

    /// KOVA INVARIANTTI: pinolla EI OLE apply-polkua. Tämä testi dokumentoi
    /// rakenteellisen takuun — proposalin elinkaari on data-only, ja ainoat
    /// mutaatiot ovat status-merkinnät. Mikään julkinen metodi ei muuta taitoa,
    /// käytäntöä tai oikeutta. (Jos joku lisää `apply`-metodin, tämä kommentti +
    /// PR-katselmointi on portti.)
    #[test]
    fn store_has_no_apply_path_only_records_and_marks_status() {
        let mut store = ProposalStore::new();
        let id = store.record(sample());
        // Ainoat tila-mutaatiot: approve/deny. Hyväksyntä EI sovella → status
        // on Approved mutta mitään ulkoista ei muutu (tämä crate ei kosketa
        // mitään taito-/käytäntö-/oikeuspintaa).
        store.approve(id);
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
    }
}
