//! Olennot (beings) busissa: rekisteröintitiedot ja vastaanottajan tyyppi.
//!
//! Busiin liittyvä olento on **Ractor-actor**, jonka viestityyppi on
//! [`ResonanceMessage`]. Bus pitää kirjaa liittyneistä olennoista
//! ([`BeingInfo`]) ja toimittaa heille viestit `cast`-kutsuilla. Yhden
//! olennon kaatuminen ei kaada busia (supervision; ks. [`crate::bus`]).
//!
//! Tämä moduuli tarjoaa myös valmiin [`CollectorBeing`]-actorin, joka kerää
//! vastaanottamansa viestit. Se on tarkoitettu testeihin ja esimerkkeihin —
//! todelliset perheenjäsenet (KERROS B) toteuttavat oman actorinsa, joka
//! reagoi sisarusten resonanssiin.

use std::sync::{Arc, Mutex};

use ractor::{Actor, ActorProcessingErr, ActorRef};

use crate::message::{BeingId, ResonanceMessage};

/// Busiin rekisteröidyn olennon metatiedot.
///
/// Kantaa olennotunnisteen, ihmisluettavan nimen ja [`ActorRef`]-viitteen,
/// jonka kautta bus toimittaa viestit. Viite on tyypitetty
/// [`ResonanceMessage`]:lle, joten olennot voivat ottaa vastaan vain busin
/// kieltä.
#[derive(Clone)]
pub struct BeingInfo {
    /// Olennon tunniste.
    id: BeingId,
    /// Olennon näyttönimi (geneerinen, esim. `"agent_a"`).
    name: String,
    /// Postilaatikko, johon viestit toimitetaan.
    inbox: ActorRef<ResonanceMessage>,
}

impl BeingInfo {
    /// Rakentaa rekisteröintitiedon.
    #[must_use]
    pub fn new(id: BeingId, name: impl Into<String>, inbox: ActorRef<ResonanceMessage>) -> Self {
        Self {
            id,
            name: name.into(),
            inbox,
        }
    }

    /// Olennon tunniste.
    #[must_use]
    pub const fn id(&self) -> BeingId {
        self.id
    }

    /// Olennon näyttönimi.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Olennon postilaatikko (toimitusosoite).
    #[must_use]
    pub fn inbox(&self) -> &ActorRef<ResonanceMessage> {
        &self.inbox
    }
}

impl std::fmt::Debug for BeingInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BeingInfo")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Kevyt, sarjallistuva tilannekuva olennosta — [`BeingInfo`] ilman
/// actor-viitettä.
///
/// Tätä palauttaa [`crate::bus::BusHandle::beings`]: se kertoo *kuka* on
/// liittynyt ilman että paljastaa sisäistä actor-koneistoa. **Tämän listan
/// EI saa olla tyhjä kun olentoja on liittynyt** — se on suoraan korjaus
/// live-3500-busin `beings:[]`-tyhjyyteen (design §2.2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BeingSnapshot {
    /// Olennon tunniste.
    pub id: BeingId,
    /// Olennon näyttönimi.
    pub name: String,
}

impl From<&BeingInfo> for BeingSnapshot {
    fn from(info: &BeingInfo) -> Self {
        Self {
            id: info.id,
            name: info.name.clone(),
        }
    }
}

/// Jaettu, säieturvallinen loki vastaanotetuista viesteistä.
///
/// [`CollectorBeing`] kirjoittaa tähän, ja testit/esimerkit lukevat sen.
pub type CollectedLog = Arc<Mutex<Vec<ResonanceMessage>>>;

/// Valmis olento-actor, joka kerää vastaanottamansa [`ResonanceMessage`]:t
/// jaettuun lokiin.
///
/// Tarkoitettu testeihin ja esimerkkeihin. Tuotannossa perheenjäsen
/// toteuttaa oman [`Actor`]-toteutuksensa, joka reagoi resonanssiin (esim.
/// päivittää omaa tunnetilaansa naapurin pulssin perusteella — affective
/// contagion).
pub struct CollectorBeing;

/// [`CollectorBeing`]:n tila: jaettu loki johon viestit kertyvät.
pub struct CollectorState {
    /// Loki johon vastaanotetut viestit kirjoitetaan.
    pub log: CollectedLog,
}

impl Actor for CollectorBeing {
    type Msg = ResonanceMessage;
    type State = CollectorState;
    type Arguments = CollectedLog;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        log: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(CollectorState { log })
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        // Lukko voi olla myrkytetty vain jos jokin toinen säie panikoi sitä
        // pidellessään; siinä tapauksessa otamme datan silti talteen sen
        // sijaan että levittäisimme paniikin tähän actoriin.
        let mut guard = match state.log.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.push(message);
        Ok(())
    }
}

impl CollectorBeing {
    /// Luo jaetun lokin [`CollectorBeing`]-actorille.
    #[must_use]
    pub fn new_log() -> CollectedLog {
        Arc::new(Mutex::new(Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::BusMessage;

    #[tokio::test]
    async fn collector_records_messages() {
        let log = CollectorBeing::new_log();
        let (actor, handle) = Actor::spawn(None, CollectorBeing, log.clone())
            .await
            .expect("spawn collector");

        let env = ResonanceMessage::new(BeingId::new(), BusMessage::text("hi"));
        actor.cast(env.clone()).expect("cast to collector");

        // Anna actorin käsitellä jonossa oleva viesti. (Stop-signaali voi
        // ohittaa tavalliset viestit, joten emme luota pelkkään stop+join
        // -järjestykseen viestin perillemenon todentamiseen.)
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        {
            let recorded = log.lock().expect("lock");
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0], env);
        } // lukko vapautetaan ennen .await:ia

        actor.stop(None);
        handle.await.expect("join");
    }

    #[tokio::test]
    async fn being_info_exposes_fields_and_snapshot() {
        let log = CollectorBeing::new_log();
        let (actor, handle) = Actor::spawn(None, CollectorBeing, log)
            .await
            .expect("spawn");
        let id = BeingId::new();
        let info = BeingInfo::new(id, "agent_a", actor.clone());

        assert_eq!(info.id(), id);
        assert_eq!(info.name(), "agent_a");

        let snap = BeingSnapshot::from(&info);
        assert_eq!(snap.id, id);
        assert_eq!(snap.name, "agent_a");

        // Debug ei panikoi eikä vuoda actor-sisäistä tilaa.
        let dbg = format!("{info:?}");
        assert!(dbg.contains("agent_a"));

        actor.stop(None);
        handle.await.expect("join");
    }

    #[test]
    fn being_snapshot_serde_roundtrip() {
        let snap = BeingSnapshot {
            id: BeingId::new(),
            name: "agent_b".into(),
        };
        let json = serde_json::to_string(&snap).expect("ser");
        let back: BeingSnapshot = serde_json::from_str(&json).expect("de");
        assert_eq!(snap, back);
    }
}
