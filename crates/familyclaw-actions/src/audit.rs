//! Audit-loki: tamper-evident tapahtumaketju toimintopinon vaiheista
//! (havainto, suunnitelma, hyväksyntä, suoritus, todiste) (KERROS A).
//!
//! Tämä moduuli määrittelee:
//! - [`AuditAction`] — mitä tapahtui (hyväksyntä myönnettiin/kulutettiin/…),
//! - [`ActionAuditEvent`] — yksittäinen lokitapahtuma (tunniste, hetki, syy),
//! - [`AuditLog`] — in-memory append-only -loki tapahtumille.
//!
//! ## Determinismi
//! Tapahtumat ottavat aikaleiman injektoituna
//! ([`familyclaw_core::time::Timestamp`]) — kelloa ei lueta tämän moduulin
//! logiikan sisällä, jotta testit ja replay pysyvät deterministisinä.
//!
//! ## OSS-raja
//! Tapahtumat eivät sisällä salaisuuksia: vapaamuotoinen `reason`-kenttä on
//! tarkoitettu lyhyelle ihmisluettavalle selitteelle, ei payloadille tai
//! avaimille.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use familyclaw_core::time::Timestamp;

use crate::ids::{ActionId, ApprovalId, AuditEventId};

/// Moduulin valmiusaste — säilytetään, jotta [`crate::all_modules_scaffolded`]
/// kääntyy edelleen muiden vielä luurankovaiheessa olevien moduulien rinnalla.
pub(crate) const SCAFFOLDED: bool = true;

/// Audit-tapahtuman tyyppi: mitä toimintopinossa tapahtui.
///
/// Sarjallistuu `snake_case`-muotoon, jotta lokia voi suodattaa ja lukea
/// koneellisesti.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// Hyväksyntä myönnettiin (human-in-the-loop hyväksyi toiminnon).
    ApprovalGranted,
    /// Hyväksyntä kulutettiin onnistuneesti (kertakäyttö käytetty).
    ApprovalConsumed,
    /// Hyväksyntä evättiin (ihminen kieltäytyi).
    ApprovalDenied,
    /// Hyväksyntä todettiin vanhentuneeksi kulutusyrityksen yhteydessä.
    ApprovalExpired,
    /// Hyväksynnän kulutus epäonnistui (esim. payload-tiiviste ei täsmännyt
    /// tai hyväksyntää oli jo käytetty).
    ApprovalRejected,
}

/// Yksittäinen audit-lokin tapahtuma toimintopinon hyväksyntävaiheesta.
///
/// Jokainen tapahtuma kantaa oman tunnisteensa, kohteena olevan toiminnon
/// ([`ActionId`]) ja mahdollisen hyväksynnän ([`ApprovalId`]) tunnisteen, sekä
/// aikaleiman ja lyhyen ihmisluettavan syyn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionAuditEvent {
    /// Tapahtuman yksilöivä tunniste.
    pub id: AuditEventId,
    /// Tapahtuman tyyppi (mitä tapahtui).
    pub action: AuditAction,
    /// Toiminto johon tapahtuma liittyy.
    pub action_id: ActionId,
    /// Hyväksynnän tunniste jos tapahtuma koskee tiettyä hyväksyntää
    /// (`None` esim. eväyksessä ennen kuin hyväksyntää on olemassa).
    pub approval_id: Option<ApprovalId>,
    /// Tapahtuman hetki (injektoitu — ei luettu kellosta).
    pub at: Timestamp,
    /// Lyhyt ihmisluettava selite (EI salaisuuksia eikä payloadia).
    pub reason: String,
}

impl ActionAuditEvent {
    /// Rakentaa uuden audit-tapahtuman tuoreella tunnisteella.
    ///
    /// Aikaleima ja syy annetaan kutsujalta; tunniste generoidaan satunnaisesti
    /// ([`AuditEventId::new`]).
    #[must_use]
    pub fn new(
        action: AuditAction,
        action_id: ActionId,
        approval_id: Option<ApprovalId>,
        at: Timestamp,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id: AuditEventId::new(),
            action,
            action_id,
            approval_id,
            at,
            reason: reason.into(),
        }
    }
}

/// In-memory append-only -audit-loki.
///
/// Loki on tarkoituksella vain lisäävä (`append`): tapahtumia ei poisteta eikä
/// muokata, mikä tukee tamper-evident-ominaisuutta. Tämä KERROS A -toteutus
/// säilyttää tapahtumat muistissa; pysyvä tallennus on substraattikerroksen
/// vastuulla.
#[derive(Debug, Clone, Default)]
pub struct AuditLog {
    /// Tapahtumat kronologisessa lisäysjärjestyksessä.
    events: Vec<ActionAuditEvent>,
}

impl AuditLog {
    /// Luo uuden tyhjän audit-lokin.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lisää tapahtuman lokin loppuun ja palauttaa lisätyn tapahtuman tunnisteen.
    pub fn append(&mut self, event: ActionAuditEvent) -> AuditEventId {
        let id = event.id;
        self.events.push(event);
        id
    }

    /// Lokissa olevien tapahtumien lukumäärä.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Onko loki tyhjä.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Kaikki tapahtumat lisäysjärjestyksessä.
    #[must_use]
    pub fn events(&self) -> &[ActionAuditEvent] {
        &self.events
    }

    /// Tietyn toiminnon ([`ActionId`]) kaikki tapahtumat lisäysjärjestyksessä.
    #[must_use]
    pub fn events_for(&self, action_id: ActionId) -> Vec<&ActionAuditEvent> {
        self.events
            .iter()
            .filter(|e| e.action_id == action_id)
            .collect()
    }

    /// Sisältääkö loki vähintään yhden annetun tyypin tapahtuman.
    #[must_use]
    pub fn contains_action(&self, action: AuditAction) -> bool {
        self.events.iter().any(|e| e.action == action)
    }
}

/// Suorituspinon (executor → verify → proof) audit-tapahtuman tyyppi.
///
/// Laajempi kuin [`AuditAction`], joka kattaa vain hyväksyntävaiheen.
/// `AuditKind` kuvaa koko toimintopinon kannalta kiinnostavat tapahtumat:
/// hyväksyntä, suoritus, redaktointi, taint-merkintä ja käytäntöeväys.
/// Sarjallistuu `snake_case`-muotoon koneellista suodatusta varten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    /// Hyväksyntä myönnettiin.
    ApprovalGranted,
    /// Hyväksyntä kulutettiin (kertakäyttö käytetty).
    ApprovalConsumed,
    /// Hyväksyntä evättiin.
    ApprovalDenied,
    /// Hyväksyntä todettiin vanhentuneeksi.
    ApprovalExpired,
    /// Toiminnon suoritus alkoi.
    ActionStarted,
    /// Toiminnon suoritus onnistui.
    ActionSucceeded,
    /// Toiminnon suoritus epäonnistui.
    ActionFailed,
    /// Salaisuudelta näyttäviä arvoja redaktoitiin todisteesta.
    RedactionApplied,
    /// Tuloste merkittiin epäluotettavaksi (taint).
    TaintMarked,
    /// Käytäntö (policy) esti toiminnon.
    PolicyDenied,
}

/// Suorituspinon yksittäinen audit-tapahtuma.
///
/// Toisin kuin [`ActionAuditEvent`] (hyväksyntäkohtainen), tämä kuvaa minkä
/// tahansa [`AuditKind`]-tapahtuman vapaamuotoisella `detail`-selitteellä.
///
/// ## OSS-raja
/// `detail` on tarkoitettu lyhyelle ihmisluettavalle selitteelle — **ei koskaan
/// raakaa salaisuutta, tokenia eikä payloadia**. Salaiset arvot redaktoidaan
/// todistepaketissa ([`crate::proof`]), eivät päädy tähän kenttään.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecAuditEvent {
    /// Tapahtuman yksilöivä tunniste.
    pub id: AuditEventId,
    /// Tapahtuman tyyppi.
    pub kind: AuditKind,
    /// Tapahtuman hetki (injektoitu — ei luettu kellosta).
    pub at: Timestamp,
    /// Toiminto johon tapahtuma liittyy.
    pub action_id: ActionId,
    /// Lyhyt ihmisluettava selite (EI raakoja salaisuuksia).
    pub detail: String,
}

impl ExecAuditEvent {
    /// Rakentaa uuden suorituspinon audit-tapahtuman tuoreella tunnisteella.
    ///
    /// Aikaleima ja selite annetaan kutsujalta; tunniste generoidaan
    /// satunnaisesti ([`AuditEventId::new`]).
    #[must_use]
    pub fn new(
        kind: AuditKind,
        action_id: ActionId,
        at: Timestamp,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: AuditEventId::new(),
            kind,
            at,
            action_id,
            detail: detail.into(),
        }
    }
}

/// Säikeenturvallinen in-memory-keräin suorituspinon audit-tapahtumille.
///
/// Tapahtumat säilytetään [`std::sync::Mutex`]-lukon takana, jotta keräintä voi
/// jakaa rinnakkaisten suoritusten kesken. Keräin on vain lisäävä: tapahtumia
/// ei poisteta eikä muokata (tamper-evident). Pysyvä tallennus on
/// substraattikerroksen vastuulla.
#[derive(Debug, Default)]
pub struct AuditCollector {
    /// Tapahtumat lisäysjärjestyksessä, lukon takana.
    events: Mutex<Vec<ExecAuditEvent>>,
}

impl AuditCollector {
    /// Luo uuden tyhjän keräimen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Kirjaa tapahtuman ja palauttaa sen tunnisteen.
    ///
    /// Lukon myrkyttyessä (paniikki toisessa säikeessä) lukko palautetaan
    /// kunnolla ([`std::sync::PoisonError::into_inner`]) tiedonhukan
    /// välttämiseksi — kirjaus ei koskaan paniikkaa.
    pub fn record(&self, event: ExecAuditEvent) -> AuditEventId {
        let id = event.id;
        let mut guard = match self.events.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.push(event);
        id
    }

    /// Palauttaa kaikki kirjatut tapahtumat lisäysjärjestyksessä (kopio).
    #[must_use]
    pub fn list(&self) -> Vec<ExecAuditEvent> {
        match self.events.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Kirjattujen tapahtumien lukumäärä.
    #[must_use]
    pub fn len(&self) -> usize {
        match self.events.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }

    /// Onko keräin tyhjä.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time::from_unix_secs;

    fn ts() -> Timestamp {
        from_unix_secs(1_700_000_000).expect("valid unix seconds")
    }

    #[test]
    fn audit_kind_serde_snake_case() {
        let json = serde_json::to_string(&AuditKind::ActionSucceeded).expect("serialize");
        assert_eq!(json, "\"action_succeeded\"");
        let back: AuditKind = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, AuditKind::ActionSucceeded);
    }

    #[test]
    fn collector_record_and_list() {
        let collector = AuditCollector::new();
        assert!(collector.is_empty());
        let action_id = ActionId::new();
        let id = collector.record(ExecAuditEvent::new(
            AuditKind::ActionStarted,
            action_id,
            ts(),
            "aloitettu",
        ));
        assert_eq!(collector.len(), 1);
        assert!(!collector.is_empty());
        let listed = collector.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(listed[0].kind, AuditKind::ActionStarted);
        assert_eq!(listed[0].action_id, action_id);
    }

    #[test]
    fn audit_action_serde_snake_case() {
        let json = serde_json::to_string(&AuditAction::ApprovalGranted).expect("serialize");
        assert_eq!(json, "\"approval_granted\"");
        let back: AuditAction = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, AuditAction::ApprovalGranted);
    }

    #[test]
    fn append_increments_len_and_returns_id() {
        let mut log = AuditLog::new();
        assert!(log.is_empty());
        let action_id = ActionId::new();
        let event = ActionAuditEvent::new(
            AuditAction::ApprovalGranted,
            action_id,
            None,
            ts(),
            "myönnetty",
        );
        let id = log.append(event);
        assert_eq!(log.len(), 1);
        assert!(!log.is_empty());
        assert_eq!(log.events()[0].id, id);
    }

    #[test]
    fn events_for_filters_by_action_id() {
        let mut log = AuditLog::new();
        let a = ActionId::new();
        let b = ActionId::new();
        log.append(ActionAuditEvent::new(
            AuditAction::ApprovalGranted,
            a,
            None,
            ts(),
            "a granted",
        ));
        log.append(ActionAuditEvent::new(
            AuditAction::ApprovalGranted,
            b,
            None,
            ts(),
            "b granted",
        ));
        assert_eq!(log.events_for(a).len(), 1);
        assert_eq!(log.events_for(a)[0].action_id, a);
    }

    #[test]
    fn contains_action_detects_presence() {
        let mut log = AuditLog::new();
        assert!(!log.contains_action(AuditAction::ApprovalDenied));
        log.append(ActionAuditEvent::new(
            AuditAction::ApprovalDenied,
            ActionId::new(),
            None,
            ts(),
            "denied",
        ));
        assert!(log.contains_action(AuditAction::ApprovalDenied));
    }
}
