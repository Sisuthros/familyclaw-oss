//! Hyväksyntäkerros (human-in-the-loop): hyväksyntäpyynnöt, niiden TTL,
//! kertakäyttöisyys (nonce) ja payload-tiivisteen sidonta (KERROS A).
//!
//! Tämä moduuli toteuttaa toimintopinon `request approval` -vaiheen:
//! - [`Approval`] — yksittäinen myönnetty hyväksyntä (TTL + payload-sidonta),
//! - [`ApprovalLedger`] — in-memory-rekisteri joka myöntää, kuluttaa ja evää
//!   hyväksyntöjä sekä kirjaa jokaisen tapahtuman audit-lokiin ([`crate::audit`]).
//!
//! ## Turvallisuusperiaatteet
//! - **Fail-closed:** kulutus epäonnistuu jos hyväksyntää ei löydy, se on
//!   vanhentunut, jo kulutettu tai payload-tiiviste ei täsmää.
//! - **Kertakäyttö (nonce):** hyväksynnän voi kuluttaa täsmälleen kerran.
//! - **Payload-sidonta:** hyväksyntä sidotaan toiminnon payloadin
//!   SHA-256-tiivisteeseen; esitetty payload tiivistetään uudelleen ja
//!   verrataan tallennettuun **vakioaikaisella** vertailulla (timing-side-channel
//!   estetään).
//!
//! ## Determinismi
//! Logiikka ottaa aikaleiman injektoituna
//! ([`familyclaw_core::time::Timestamp`]) — kelloa ei lueta tämän moduulin
//! sisällä, jotta testit ja replay pysyvät deterministisinä.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};

use familyclaw_core::time::Timestamp;

use crate::audit::{ActionAuditEvent, AuditAction, AuditLog};
use crate::error::{ActionError, Result};
use crate::ids::{ActionId, ApprovalId};

/// Moduulin valmiusaste — säilytetään, jotta [`crate::all_modules_scaffolded`]
/// kääntyy edelleen muiden vielä luurankovaiheessa olevien moduulien rinnalla.
pub(crate) const SCAFFOLDED: bool = true;

/// Laskee annetun payloadin SHA-256-tiivisteen heksamerkkijonona.
///
/// Käytetään hyväksynnän sitomiseen tiettyyn payloadiin: hyväksyntä koskee vain
/// täsmälleen sitä payloadia jonka tiiviste tallennettiin myöntöhetkellä.
#[must_use]
pub fn sha256_hex(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        // write! Stringiin ei voi epäonnistua → virhe poltetaan tarkoituksella.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Laskee vanhentumishetken `now + ttl` **kyllästyen** (saturating) ylivuodon
/// yli, jottei myöntö koskaan panikoi äärimmäisellä TTL:llä.
///
/// `chrono`-kirjaston `DateTime + Duration` panikoi jos tulos ylittää
/// edustettavan aika-alueen ([`DateTime::<Utc>::MAX_UTC`] /
/// [`DateTime::<Utc>::MIN_UTC`]). Tämä funktio käyttää tarkistettua
/// yhteenlaskua ja kyllästää rajalle:
/// - **Positiivinen ylivuoto** (valtava positiivinen TTL) → [`DateTime::<Utc>::MAX_UTC`]
///   (kutsujan tarkoitus "ei käytännössä koskaan vanhene" säilyy ilman kaatumista).
/// - **Negatiivinen alivuoto** (valtava negatiivinen TTL) → [`DateTime::<Utc>::MIN_UTC`]
///   (jo vanhentunut — kulutus epäonnistuu fail-closed, kuten negatiivisella
///   TTL:llä kuuluukin).
///
/// Näin vanhentumislogiikka pysyy **fail-closed** myös ääritilanteissa: kyllästys
/// ei koskaan tee jo vanhentuneesta hyväksynnästä elävää.
#[must_use]
fn saturating_expiry(now: Timestamp, ttl: Duration) -> Timestamp {
    match now.checked_add_signed(ttl) {
        Some(ts) => ts,
        // None = ylivuoto. Suunta päätellään TTL:n etumerkistä.
        None if ttl < Duration::zero() => DateTime::<Utc>::MIN_UTC,
        None => DateTime::<Utc>::MAX_UTC,
    }
}

/// Vertaa kahta heksatiivistettä vakioaikaisesti (timing-side-channel suoja).
///
/// Eripituiset syötteet eivät koskaan täsmää, ja vertailu käy aina kaikki
/// tavut läpi pituuden salliessa, jotta vertailuun kuluva aika ei vuoda tietoa
/// siitä, kuinka monta etumerkkiä täsmäsi.
#[must_use]
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Yksittäinen myönnetty hyväksyntä (human-in-the-loop).
///
/// Hyväksyntä on TTL-rajattu, kertakäyttöinen ([`Approval::consumed`]) ja sidottu
/// toiminnon payloadin SHA-256-tiivisteeseen ([`Approval::payload_hash`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    /// Hyväksynnän yksilöivä tunniste.
    pub id: ApprovalId,
    /// Toiminto jonka tämä hyväksyntä valtuuttaa.
    pub action_id: ActionId,
    /// Valtuutetun payloadin SHA-256-tiiviste heksamuodossa.
    pub payload_hash: String,
    /// Hetki jolloin hyväksyntä myönnettiin.
    pub granted_at: Timestamp,
    /// Hetki jonka jälkeen hyväksyntä on vanhentunut (`granted_at + ttl`).
    pub expires_at: Timestamp,
    /// Onko hyväksyntä jo kulutettu (kertakäyttö — `true` estää uudelleenkäytön).
    pub consumed: bool,
}

impl Approval {
    /// Onko hyväksyntä vanhentunut annettuun hetkeen `now` nähden.
    ///
    /// Vanhentunut tarkoittaa, että `now` on aidosti myöhäisempi kuin
    /// [`Approval::expires_at`].
    #[must_use]
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now > self.expires_at
    }
}

/// In-memory-rekisteri hyväksynnöille (KERROS A).
///
/// Säilyttää hyväksynnät tunnisteen mukaan ja kytkee oman audit-lokin
/// ([`AuditLog`]) johon jokainen myöntö, kulutus, eväys ja vanhentuminen
/// kirjataan. Pysyvä tallennus on substraattikerroksen vastuulla.
#[derive(Debug, Clone, Default)]
pub struct ApprovalLedger {
    /// Tunniste → hyväksyntä -kartta.
    approvals: HashMap<ApprovalId, Approval>,
    /// Audit-loki johon hyväksyntätapahtumat kirjataan.
    audit: AuditLog,
}

impl ApprovalLedger {
    /// Luo uuden tyhjän hyväksyntärekisterin.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Myöntää hyväksynnän toiminnolle ja sitoo sen payload-tiivisteeseen.
    ///
    /// Hyväksyntä vanhenee `now + ttl` -hetkellä. Myönnöstä kirjataan
    /// [`AuditAction::ApprovalGranted`]-tapahtuma audit-lokiin. Palauttaa
    /// myönnetyn hyväksynnän kopion.
    ///
    /// `ttl` saa olla nolla (hyväksyntä vanhenee heti seuraavalla hetkellä) tai
    /// negatiivinen (jo valmiiksi vanhentunut); kumpaakaan ei pidetä virheenä —
    /// fail-closed-logiikka hoitaa kulutuksen eston.
    ///
    /// Vanhentumishetki lasketaan **kyllästyen** (saturating): äärimmäinen
    /// TTL ei koskaan panikoi (toisin kuin suora `now + ttl`). Ylivuoto kyllästyy
    /// [`DateTime::<Utc>::MAX_UTC`]:hin ja alivuoto [`DateTime::<Utc>::MIN_UTC`]:hin,
    /// joten vanhentumislogiikka pysyy fail-closed myös rajatapauksissa.
    pub fn grant(
        &mut self,
        action_id: ActionId,
        payload_hash: impl Into<String>,
        now: Timestamp,
        ttl: Duration,
    ) -> Approval {
        let approval = Approval {
            id: ApprovalId::new(),
            action_id,
            payload_hash: payload_hash.into(),
            granted_at: now,
            expires_at: saturating_expiry(now, ttl),
            consumed: false,
        };
        self.audit.append(ActionAuditEvent::new(
            AuditAction::ApprovalGranted,
            action_id,
            Some(approval.id),
            now,
            "hyväksyntä myönnetty",
        ));
        self.approvals.insert(approval.id, approval.clone());
        approval
    }

    /// Kuluttaa hyväksynnän esitettyä payloadia vasten (kertakäyttö).
    ///
    /// Vaiheet (fail-closed):
    /// 1. Hyväksyntää ei löydy → [`ActionError::ApprovalMissing`].
    /// 2. Hyväksyntä on vanhentunut (`now > expires_at`) →
    ///    [`ActionError::ApprovalExpired`].
    /// 3. Hyväksyntä on jo kulutettu → [`ActionError::ApprovalReused`].
    /// 4. Esitetyn payloadin SHA-256-tiiviste ei vastaa tallennettua
    ///    (vakioaikainen vertailu) → [`ActionError::ApprovalPayloadMismatch`].
    /// 5. Onnistuessa hyväksyntä merkitään kulutetuksi ([`Approval::consumed`])
    ///    ja kirjataan [`AuditAction::ApprovalConsumed`].
    ///
    /// Jokainen epäonnistuminen kirjataan myös audit-lokiin
    /// ([`AuditAction::ApprovalExpired`] tai [`AuditAction::ApprovalRejected`]).
    ///
    /// # Errors
    /// Palauttaa edellä kuvatun [`ActionError`]-variantin jos jokin tarkistus ei
    /// läpäise.
    pub fn consume(
        &mut self,
        approval_id: ApprovalId,
        action_payload: &[u8],
        now: Timestamp,
    ) -> Result<()> {
        // (a) Fail-closed: löytymätöntä hyväksyntää ei voi kuluttaa.
        let Some(approval) = self.approvals.get(&approval_id) else {
            return Err(ActionError::ApprovalMissing(approval_id.to_string()));
        };
        let action_id = approval.action_id;

        // (b) Vanhentunut?
        if approval.is_expired(now) {
            self.audit.append(ActionAuditEvent::new(
                AuditAction::ApprovalExpired,
                action_id,
                Some(approval_id),
                now,
                "hyväksyntä vanhentunut kulutusyrityksessä",
            ));
            return Err(ActionError::ApprovalExpired(approval_id.to_string()));
        }

        // (d) Jo kulutettu? (kertakäyttö)
        if approval.consumed {
            self.audit.append(ActionAuditEvent::new(
                AuditAction::ApprovalRejected,
                action_id,
                Some(approval_id),
                now,
                "hyväksyntä jo kulutettu (uudelleenkäyttö estetty)",
            ));
            return Err(ActionError::ApprovalReused(approval_id.to_string()));
        }

        // (c) Payload-tiiviste täsmää? (vakioaikainen vertailu)
        let presented_hash = sha256_hex(action_payload);
        if !constant_time_eq(&presented_hash, &approval.payload_hash) {
            self.audit.append(ActionAuditEvent::new(
                AuditAction::ApprovalRejected,
                action_id,
                Some(approval_id),
                now,
                "payload-tiiviste ei vastaa hyväksyntää",
            ));
            return Err(ActionError::ApprovalPayloadMismatch(
                approval_id.to_string(),
            ));
        }

        // (e) Onnistuu → merkitse kulutetuksi (ONE-SHOT) ja kirjaa.
        if let Some(stored) = self.approvals.get_mut(&approval_id) {
            stored.consumed = true;
        }
        self.audit.append(ActionAuditEvent::new(
            AuditAction::ApprovalConsumed,
            action_id,
            Some(approval_id),
            now,
            "hyväksyntä kulutettu",
        ));
        Ok(())
    }

    /// Kirjaa eväyksen: ihminen kieltäytyi valtuuttamasta toimintoa.
    ///
    /// Eväys ei vaadi olemassa olevaa hyväksyntää — se kirjaa pelkän
    /// [`AuditAction::ApprovalDenied`]-tapahtuman annetulla syyllä. Palauttaa
    /// kirjatun audit-tapahtuman.
    pub fn deny(
        &mut self,
        action_id: ActionId,
        reason: impl Into<String>,
        now: Timestamp,
    ) -> ActionAuditEvent {
        let event =
            ActionAuditEvent::new(AuditAction::ApprovalDenied, action_id, None, now, reason);
        self.audit.append(event.clone());
        event
    }

    /// Hakee hyväksynnän tunnisteella (vain luku); `None` jos ei löydy.
    #[must_use]
    pub fn get(&self, approval_id: ApprovalId) -> Option<&Approval> {
        self.approvals.get(&approval_id)
    }

    /// Pääsy audit-lokiin (vain luku).
    #[must_use]
    pub fn audit_log(&self) -> &AuditLog {
        &self.audit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{required_approval, ActionRisk, ApprovalPolicy, ApprovalRequirement};
    use familyclaw_core::time::from_unix_secs;

    fn at(secs: i64) -> Timestamp {
        from_unix_secs(secs).expect("valid unix seconds")
    }

    fn payload(label: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({ "channel": "general", "body": label }))
            .expect("serialize payload")
    }

    #[test]
    fn sha256_hex_is_stable_and_hex() {
        let h = sha256_hex(b"agent_a");
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(h, sha256_hex(b"agent_a"));
        assert_ne!(h, sha256_hex(b"agent_b"));
    }

    #[test]
    fn constant_time_eq_matches_only_identical() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(constant_time_eq("", ""));
    }

    /// REQUIRED: `write_external` on estetty ilman hyväksyntää — käytäntö vaatii
    /// hyväksynnän ja kulutus ilman myöntöä epäonnistuu fail-closed.
    #[test]
    fn write_external_blocked_without_approval() {
        // Policy sanoo: hyväksyntä vaaditaan.
        let req = required_approval(ActionRisk::WriteExternal, ApprovalPolicy::AutoIfReadOnly);
        assert_eq!(req, ApprovalRequirement::RequireApproval);

        // Yritetään kuluttaa hyväksyntä jota ei ole myönnetty → fail closed.
        let mut ledger = ApprovalLedger::new();
        let phantom = ApprovalId::new();
        let err = ledger
            .consume(phantom, &payload("send to agent_b"), at(1_700_000_000))
            .expect_err("consume without grant must fail closed");
        assert!(matches!(err, ActionError::ApprovalMissing(_)));
        // Audit-loki ei sisällä kulutusta.
        assert!(!ledger
            .audit_log()
            .contains_action(AuditAction::ApprovalConsumed));
    }

    /// REQUIRED: hyväksyntä sallii täsmälleen yhden suorituksen — toinen kulutus
    /// epäonnistuu (jo kulutettu).
    #[test]
    fn approval_permits_exactly_one_execution() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("notify agent_b");
        let hash = sha256_hex(&body);

        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::minutes(5));

        // Ensimmäinen kulutus onnistuu.
        ledger
            .consume(granted.id, &body, at(1_700_000_010))
            .expect("first consume succeeds");
        assert!(ledger.get(granted.id).expect("present").consumed);

        // Toinen kulutus epäonnistuu — kertakäyttö.
        let err = ledger
            .consume(granted.id, &body, at(1_700_000_020))
            .expect_err("second consume must fail");
        assert!(matches!(err, ActionError::ApprovalReused(_)));

        // Tasan yksi onnistunut kulutus kirjattu.
        let consumed = ledger
            .audit_log()
            .events()
            .iter()
            .filter(|e| e.action == AuditAction::ApprovalConsumed)
            .count();
        assert_eq!(consumed, 1);
    }

    /// REQUIRED: hyväksyntää ei voi käyttää uudelleen muutetulla payloadilla —
    /// eri payload-tiiviste estää kulutuksen.
    #[test]
    fn approval_cannot_be_reused_with_changed_payload() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let original = payload("transfer to user-42");
        let hash = sha256_hex(&original);

        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::minutes(5));

        // Esitetään ERI payload kuin hyväksytty.
        let tampered = payload("transfer to attacker");
        let err = ledger
            .consume(granted.id, &tampered, at(1_700_000_010))
            .expect_err("changed payload must fail");
        assert!(matches!(err, ActionError::ApprovalPayloadMismatch(_)));

        // Hyväksyntä EI kulunut (alkuperäinen payload voi yhä onnistua).
        assert!(!ledger.get(granted.id).expect("present").consumed);
        ledger
            .consume(granted.id, &original, at(1_700_000_020))
            .expect("original payload still consumes");
    }

    /// REQUIRED: vanhentunut hyväksyntä estää suorituksen — kun `now` ylittää
    /// `expires_at`.
    #[test]
    fn expired_approval_blocks_execution() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("send to agent_b");
        let hash = sha256_hex(&body);

        // TTL 60 sekuntia.
        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::seconds(60));

        // now = granted_at + 120s > expires_at → vanhentunut.
        let err = ledger
            .consume(granted.id, &body, at(1_700_000_120))
            .expect_err("expired approval must block");
        assert!(matches!(err, ActionError::ApprovalExpired(_)));
        assert!(ledger
            .audit_log()
            .contains_action(AuditAction::ApprovalExpired));
        // Ei kulutettu.
        assert!(!ledger.get(granted.id).expect("present").consumed);
    }

    /// INVARIANTTI (raja): hyväksyntä on voimassa TÄSMÄLLEEN `expires_at`-hetkellä
    /// (`now == expires_at`, `>` on aito) mutta evätään heti pienimmän askeleen
    /// jälkeen (`expires_at + 1s`). Lukitsee `fail-closed`-rajan vanhentumisen
    /// jälkeen: yhtäsuuri kelpaa, aidosti myöhempi ei.
    #[test]
    fn expiry_boundary_exact_ok_then_one_step_after_blocks() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("boundary");
        let hash = sha256_hex(&body);

        // expires_at = 1_700_000_000 + 60 = 1_700_000_060.
        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::seconds(60));
        assert_eq!(granted.expires_at, at(1_700_000_060));
        assert!(
            !granted.is_expired(at(1_700_000_060)),
            "tasan rajalla EI vanhentunut"
        );
        assert!(
            granted.is_expired(at(1_700_000_061)),
            "rajan jälkeen vanhentunut"
        );

        // Pienin askel rajan JÄLKEEN → kulutus evätään vaikka payload on oikea.
        let err = ledger
            .consume(granted.id, &body, at(1_700_000_061))
            .expect_err("one second after expiry must fail closed even with correct payload");
        assert!(matches!(err, ActionError::ApprovalExpired(_)));
        assert!(!ledger.get(granted.id).expect("present").consumed);
    }

    /// INVARIANTTI (kulutusjärjestys): vanhentunut hyväksyntä evätään ENNEN
    /// payload-tarkistusta — eli "oikea payload" ei voi koskaan ohittaa
    /// vanhentumista. Lisäksi vanhentunutta hyväksyntää ei voi kuluttaa
    /// myöhemmin oikealla payloadilla edes silloin kun se ei ole kulunut.
    #[test]
    fn expired_blocks_even_with_correct_payload() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("exactly the approved payload");
        let hash = sha256_hex(&body);

        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::seconds(30));

        // OIKEA payload, mutta now on rajan jälkeen → ApprovalExpired (ei
        // PayloadMismatch eikä onnistuminen).
        let err = ledger
            .consume(granted.id, &body, at(1_700_000_031))
            .expect_err("expired must win over correct payload");
        assert!(
            matches!(err, ActionError::ApprovalExpired(_)),
            "expiry must be evaluated before payload; got {err:?}"
        );
        // EI kulutettu — eikä koskaan voi kulua oikeallakaan payloadilla rajan jälkeen.
        assert!(!ledger.get(granted.id).expect("present").consumed);
        let err2 = ledger
            .consume(granted.id, &body, at(1_700_000_999))
            .expect_err("still expired later");
        assert!(matches!(err2, ActionError::ApprovalExpired(_)));
    }

    /// INVARIANTTI (ylivuotosuoja): äärimmäinen TTL ei panikoi myöntövaiheessa.
    /// Aiempi `now + ttl` panikoi `DateTime + TimeDelta overflowed` -virheeseen;
    /// kyllästys palauttaa `MAX_UTC`:n (tuotantopolulla ei panic).
    #[test]
    fn grant_with_overflowing_ttl_saturates_instead_of_panicking() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("huge ttl");
        let hash = sha256_hex(&body);

        // Valtava positiivinen TTL → ennen korjausta tämä panikoi grant():ssa.
        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::MAX);
        assert_eq!(granted.expires_at, DateTime::<Utc>::MAX_UTC);
        // Saturoitu MAX → ei vanhennu millään realistisella now-arvolla.
        assert!(!granted.is_expired(at(1_700_000_010)));
        ledger
            .consume(granted.id, &body, at(1_700_000_010))
            .expect("non-expired saturated approval still consumable");
    }

    /// INVARIANTTI (alivuoto fail-closed): äärimmäinen NEGATIIVINEN TTL
    /// kyllästyy `MIN_UTC`:hin → hyväksyntä on jo vanhentunut → kulutus evätään.
    /// Ylivuoto ei koskaan tee jo vanhentuneesta hyväksynnästä elävää.
    #[test]
    fn grant_with_underflowing_negative_ttl_fails_closed() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("negative huge ttl");
        let hash = sha256_hex(&body);

        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::MIN);
        assert_eq!(granted.expires_at, DateTime::<Utc>::MIN_UTC);
        assert!(
            granted.is_expired(at(1_700_000_000)),
            "underflow saturates to already-expired"
        );

        let err = ledger
            .consume(granted.id, &body, at(1_700_000_000))
            .expect_err("underflowed (already expired) approval must fail closed");
        assert!(matches!(err, ActionError::ApprovalExpired(_)));
        assert!(!ledger.get(granted.id).expect("present").consumed);
    }

    /// INVARIANTTI (nolla-TTL): `ttl = 0` → `expires_at == granted_at`.
    /// Tasan myöntöhetkellä kulutus onnistuu (raja kelpaa), mutta yksikin sekunti
    /// myöhemmin se evätään.
    #[test]
    fn zero_ttl_consumable_at_grant_instant_but_not_after() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let body = payload("zero ttl");
        let hash = sha256_hex(&body);

        let granted = ledger.grant(action_id, hash, at(1_700_000_000), Duration::zero());
        assert_eq!(granted.expires_at, at(1_700_000_000));

        // Yksi sekunti myöhemmin → vanhentunut, evätään oikeallakin payloadilla.
        let err = ledger
            .consume(granted.id, &body, at(1_700_000_001))
            .expect_err("zero-ttl approval expires one step after grant");
        assert!(matches!(err, ActionError::ApprovalExpired(_)));
    }

    /// REQUIRED: eväys kirjaa audit-tapahtuman.
    #[test]
    fn denial_records_audit_event() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();

        let before = ledger.audit_log().len();
        let event = ledger.deny(action_id, "ihminen kieltäytyi", at(1_700_000_000));

        assert_eq!(event.action, AuditAction::ApprovalDenied);
        assert_eq!(event.action_id, action_id);
        assert_eq!(ledger.audit_log().len(), before + 1);
        assert!(ledger
            .audit_log()
            .contains_action(AuditAction::ApprovalDenied));
        assert_eq!(ledger.audit_log().events_for(action_id).len(), 1);
    }

    #[test]
    fn grant_records_audit_event() {
        let mut ledger = ApprovalLedger::new();
        let action_id = ActionId::new();
        let granted = ledger.grant(
            action_id,
            sha256_hex(&payload("x")),
            at(1_700_000_000),
            Duration::minutes(1),
        );
        assert!(ledger
            .audit_log()
            .contains_action(AuditAction::ApprovalGranted));
        let events = ledger.audit_log().events_for(action_id);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].approval_id, Some(granted.id));
    }
}
