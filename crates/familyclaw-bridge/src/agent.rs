//! Agenttirekisteri: agenttien tiedot, rekisteröinti, liveness ja heartbeatit.
//!
//! Tämä moduuli tarjoaa [`AgentRegistry`]:n — säieturvallisen rekisterin
//! perheen agenteista. Jokainen agentti kuvataan [`AgentInfo`]-rakenteella,
//! ja sen "elossaolo" johdetaan viimeisimmästä heartbeatista suhteessa
//! konfiguroituun aikakatkaisuun ([`Liveness`]).
//!
//! Rekisteri on tarkoituksella riippumaton kuljetuskerroksesta (ei MCP-
//! eikä HTTP-sidontaa) — adapterit kytketään myöhemmin. Sisäinen tila on suojattu
//! [`tokio::sync::RwLock`]illa, joten useat tehtävät voivat lukea
//! samanaikaisesti ja kirjoitukset sarjallistuvat.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Duration;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use familyclaw_core::ids::AgentId;
use familyclaw_core::time::{self, Timestamp};
use familyclaw_core::{FamilyClawError, Result};

/// Agentin rooli perheen työnjaossa.
///
/// Vastaa olemassa olevan family-bridge-rajapinnan roolijoukkoa, mutta
/// geneerisenä (ei sidottu yksittäisiin perheenjäseniin).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// Strategia ja koordinointi (esim. syvät analyysit).
    Strategy,
    /// Tehtävien suorittaja (koodi, toteutus).
    Executor,
    /// Tiedustelija (kevyt, utelias läsnäolo).
    Scout,
    /// Kenttäoperaattori (työpöytä-/laiteautomaatio).
    FieldOperator,
}

/// Agentin ajoympäristön tyyppi (host).
///
/// Geneerinen — ei viittaa todellisiin koneisiin tai polkuihin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    /// Paikallinen natiiviprosessi.
    Local,
    /// WSL2-ympäristö.
    Wsl,
    /// Erillinen laitteisto (hardware node).
    Hardware,
    /// "Body side" — kehollinen/perifeerinen ajoympäristö.
    BodySide,
}

/// Agentin elossaolotila johdettuna viimeisimmästä heartbeatista.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// Heartbeat on tuoreempi kuin aikakatkaisu → agentti on tavoitettavissa.
    Online,
    /// Viimeisin heartbeat on aikakatkaisua vanhempi → ei tavoitettavissa.
    Offline,
    /// Agentilta ei ole koskaan saatu heartbeatia rekisteröinnin jälkeen.
    Unknown,
}

/// Yhden agentin (perheenjäsenen) kuvaus rekisterissä.
///
/// **OSS-raja:** kentät ovat geneerisiä. Sielu/persoona/avaimet eivät kuulu
/// tähän — `preferred_model` on vain mallin nimi (esim. `"provider/model"`),
/// ei avain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    /// Agentin vakaa tunniste.
    pub id: AgentId,

    /// Näyttönimi (geneerinen, esim. `"agent_a"`).
    pub display_name: String,

    /// Rooli työnjaossa.
    pub role: AgentRole,

    /// Ajoympäristön tyyppi.
    pub host_kind: HostKind,

    /// Kyvykkyydet (geneeriset tunnisteet, esim. `"browser"`, `"system.run"`).
    #[serde(default)]
    pub capabilities: Vec<String>,

    /// Ensisijaisen mallin nimi, jos asetettu (ei avain).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_model: Option<String>,

    /// Rekisteröintihetki (UTC).
    pub registered_at: Timestamp,

    /// Viimeisimmän heartbeatin hetki (UTC), tai `None` jos ei vielä saatu.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_heartbeat: Option<Timestamp>,
}

impl AgentInfo {
    /// Rakentaa uuden agenttikuvauksen pakollisilla kentillä.
    ///
    /// `registered_at` asetetaan nykyhetkeen ja `last_heartbeat` on aluksi
    /// `None` (tila [`Liveness::Unknown`] kunnes ensimmäinen heartbeat saapuu).
    pub fn new(
        id: AgentId,
        display_name: impl Into<String>,
        role: AgentRole,
        host_kind: HostKind,
    ) -> Self {
        Self {
            id,
            display_name: display_name.into(),
            role,
            host_kind,
            capabilities: Vec::new(),
            preferred_model: None,
            registered_at: time::now(),
            last_heartbeat: None,
        }
    }

    /// Asettaa kyvykkyydet (builder-tyyli).
    #[must_use]
    pub fn with_capabilities<I, S>(mut self, capabilities: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.capabilities = capabilities.into_iter().map(Into::into).collect();
        self
    }

    /// Asettaa ensisijaisen mallin nimen (builder-tyyli).
    #[must_use]
    pub fn with_preferred_model(mut self, model: impl Into<String>) -> Self {
        self.preferred_model = Some(model.into());
        self
    }

    /// Validoi agenttikuvauksen.
    ///
    /// # Errors
    /// [`FamilyClawError::InvalidInput`] jos näyttönimi on tyhjä tai jokin
    /// kyvykkyys on tyhjä merkkijono.
    pub fn validate(&self) -> Result<()> {
        if self.display_name.trim().is_empty() {
            return Err(FamilyClawError::invalid_input(
                "agent display_name must not be empty",
            ));
        }
        if self.capabilities.iter().any(|c| c.trim().is_empty()) {
            return Err(FamilyClawError::invalid_input(
                "agent capability entries must not be empty",
            ));
        }
        Ok(())
    }

    /// Laskee agentin elossaolotilan annetulla aikakatkaisulla ja
    /// nykyhetkellä `now`.
    ///
    /// `now` annetaan parametrina determinismin vuoksi (helpottaa testausta
    /// ja durable-replayta).
    #[must_use]
    pub fn liveness_at(&self, now: Timestamp, timeout: Duration) -> Liveness {
        match self.last_heartbeat {
            None => Liveness::Unknown,
            Some(hb) => {
                if now.signed_duration_since(hb) <= timeout {
                    Liveness::Online
                } else {
                    Liveness::Offline
                }
            }
        }
    }
}

/// Säieturvallinen rekisteri perheen agenteista.
///
/// Sisältää agenttien [`AgentInfo`]-tiedot, hoitaa rekisteröinnin, haun,
/// heartbeatit ja livenessin laskennan. Aikakatkaisu ([`heartbeat_timeout`])
/// määrää milloin agentti katsotaan offline-tilaan.
///
/// [`heartbeat_timeout`]: AgentRegistry::heartbeat_timeout
#[derive(Debug, Clone)]
pub struct AgentRegistry {
    inner: Arc<RwLock<HashMap<AgentId, AgentInfo>>>,
    heartbeat_timeout: Duration,
}

/// Liveness-aikakatkaisun oletusarvo (sekunteina): 30 s.
const DEFAULT_HEARTBEAT_TIMEOUT_SECS: i64 = 30;

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentRegistry {
    /// Luo tyhjän rekisterin oletusaikakatkaisulla (30 s).
    #[must_use]
    pub fn new() -> Self {
        Self::with_timeout(Duration::seconds(DEFAULT_HEARTBEAT_TIMEOUT_SECS))
    }

    /// Luo tyhjän rekisterin annetulla heartbeat-aikakatkaisulla.
    ///
    /// Ei-positiivinen kesto normalisoidaan nollaan, jolloin agentit jotka
    /// eivät ole lähettäneet heartbeatia *tasan nyt* näkyvät offline-tilassa.
    #[must_use]
    pub fn with_timeout(heartbeat_timeout: Duration) -> Self {
        let heartbeat_timeout = if heartbeat_timeout < Duration::zero() {
            Duration::zero()
        } else {
            heartbeat_timeout
        };
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            heartbeat_timeout,
        }
    }

    /// Palauttaa rekisterin heartbeat-aikakatkaisun.
    #[must_use]
    pub fn heartbeat_timeout(&self) -> Duration {
        self.heartbeat_timeout
    }

    /// Rekisteröi agentin. Jos sama tunniste on jo rekisterissä, kuvaus
    /// korvataan (idempotentti uudelleenrekisteröinti).
    ///
    /// # Errors
    /// [`FamilyClawError::InvalidInput`] jos [`AgentInfo::validate`] epäonnistuu.
    pub async fn register(&self, info: AgentInfo) -> Result<()> {
        info.validate()?;
        let mut guard = self.inner.write().await;
        guard.insert(info.id, info);
        Ok(())
    }

    /// Poistaa agentin rekisteristä. Palauttaa poistetun kuvauksen jos se oli
    /// olemassa.
    pub async fn deregister(&self, id: AgentId) -> Option<AgentInfo> {
        let mut guard = self.inner.write().await;
        guard.remove(&id)
    }

    /// Hakee agentin kuvauksen tunnisteen perusteella.
    pub async fn get(&self, id: AgentId) -> Option<AgentInfo> {
        let guard = self.inner.read().await;
        guard.get(&id).cloned()
    }

    /// Onko annettu agentti rekisteröity.
    pub async fn contains(&self, id: AgentId) -> bool {
        let guard = self.inner.read().await;
        guard.contains_key(&id)
    }

    /// Rekisteröityjen agenttien määrä.
    pub async fn len(&self) -> usize {
        let guard = self.inner.read().await;
        guard.len()
    }

    /// Onko rekisteri tyhjä.
    pub async fn is_empty(&self) -> bool {
        let guard = self.inner.read().await;
        guard.is_empty()
    }

    /// Palauttaa kaikkien agenttien kuvaukset tunnisteen mukaan järjestettynä
    /// (deterministinen järjestys).
    pub async fn list(&self) -> Vec<AgentInfo> {
        let guard = self.inner.read().await;
        let mut out: Vec<AgentInfo> = guard.values().cloned().collect();
        out.sort_by_key(|a| a.id);
        out
    }

    /// Kirjaa heartbeatin agentille hetkellä `at`.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] jos agenttia ei ole rekisteröity.
    pub async fn heartbeat(&self, id: AgentId, at: Timestamp) -> Result<()> {
        let mut guard = self.inner.write().await;
        match guard.get_mut(&id) {
            Some(info) => {
                info.last_heartbeat = Some(at);
                Ok(())
            }
            None => Err(FamilyClawError::not_found(format!("agent {id}"))),
        }
    }

    /// Kirjaa heartbeatin nykyhetkellä.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] jos agenttia ei ole rekisteröity.
    pub async fn heartbeat_now(&self, id: AgentId) -> Result<()> {
        self.heartbeat(id, time::now()).await
    }

    /// Palauttaa agentin elossaolotilan suhteessa hetkeen `now`.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] jos agenttia ei ole rekisteröity.
    pub async fn liveness_at(&self, id: AgentId, now: Timestamp) -> Result<Liveness> {
        let guard = self.inner.read().await;
        match guard.get(&id) {
            Some(info) => Ok(info.liveness_at(now, self.heartbeat_timeout)),
            None => Err(FamilyClawError::not_found(format!("agent {id}"))),
        }
    }

    /// Palauttaa agentin elossaolotilan nykyhetkellä.
    ///
    /// # Errors
    /// [`FamilyClawError::NotFound`] jos agenttia ei ole rekisteröity.
    pub async fn liveness(&self, id: AgentId) -> Result<Liveness> {
        self.liveness_at(id, time::now()).await
    }

    /// Palauttaa kaikki agentit jotka ovat online-tilassa hetkellä `now`,
    /// tunnisteen mukaan järjestettynä.
    pub async fn online_at(&self, now: Timestamp) -> Vec<AgentInfo> {
        let guard = self.inner.read().await;
        let mut out: Vec<AgentInfo> = guard
            .values()
            .filter(|info| info.liveness_at(now, self.heartbeat_timeout) == Liveness::Online)
            .cloned()
            .collect();
        out.sort_by_key(|a| a.id);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> Timestamp {
        time::from_unix_secs(secs).expect("valid unix seconds")
    }

    fn sample(id: AgentId, name: &str) -> AgentInfo {
        AgentInfo::new(id, name, AgentRole::Executor, HostKind::Local)
    }

    #[test]
    fn agent_info_builder_sets_fields() {
        let id = AgentId::new();
        let info = sample(id, "agent_a")
            .with_capabilities(["browser", "system.run"])
            .with_preferred_model("provider/model");
        assert_eq!(info.id, id);
        assert_eq!(info.display_name, "agent_a");
        assert_eq!(info.role, AgentRole::Executor);
        assert_eq!(info.host_kind, HostKind::Local);
        assert_eq!(info.capabilities, vec!["browser", "system.run"]);
        assert_eq!(info.preferred_model.as_deref(), Some("provider/model"));
        assert!(info.last_heartbeat.is_none());
    }

    #[test]
    fn agent_info_validate_rejects_empty_name_and_capability() {
        let id = AgentId::new();
        let mut bad = sample(id, "   ");
        assert!(bad.validate().is_err());

        bad.display_name = "agent_a".into();
        bad.capabilities = vec!["ok".into(), "  ".into()];
        assert!(bad.validate().is_err());

        bad.capabilities = vec!["ok".into()];
        assert!(bad.validate().is_ok());
    }

    #[test]
    fn liveness_at_handles_never_online_offline() {
        let id = AgentId::new();
        let mut info = sample(id, "agent_a");
        let timeout = Duration::seconds(30);

        // Ei heartbeatia → Unknown.
        assert_eq!(info.liveness_at(ts(100), timeout), Liveness::Unknown);

        // Heartbeat juuri nyt → Online.
        info.last_heartbeat = Some(ts(100));
        assert_eq!(info.liveness_at(ts(100), timeout), Liveness::Online);

        // 30 s myöhemmin, rajalla → Online (<=).
        assert_eq!(info.liveness_at(ts(130), timeout), Liveness::Online);

        // 31 s myöhemmin → Offline.
        assert_eq!(info.liveness_at(ts(131), timeout), Liveness::Offline);
    }

    #[test]
    fn agent_info_serde_roundtrip() {
        let id = AgentId::new();
        let mut info = sample(id, "agent_a").with_capabilities(["x"]);
        info.last_heartbeat = Some(ts(42));
        let json = serde_json::to_string(&info).expect("serialize");
        let back: AgentInfo = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(info, back);
    }

    #[tokio::test]
    async fn register_get_and_idempotent_replace() {
        let reg = AgentRegistry::new();
        let id = AgentId::new();
        assert!(reg.is_empty().await);

        reg.register(sample(id, "agent_a")).await.expect("register");
        assert_eq!(reg.len().await, 1);
        assert!(reg.contains(id).await);
        assert_eq!(
            reg.get(id).await.map(|i| i.display_name),
            Some("agent_a".to_string())
        );

        // Uudelleenrekisteröinti samalla id:llä korvaa, ei kasvata määrää.
        reg.register(sample(id, "agent_a_renamed"))
            .await
            .expect("re-register");
        assert_eq!(reg.len().await, 1);
        assert_eq!(
            reg.get(id).await.map(|i| i.display_name),
            Some("agent_a_renamed".to_string())
        );
    }

    #[tokio::test]
    async fn register_rejects_invalid_info() {
        let reg = AgentRegistry::new();
        let id = AgentId::new();
        let err = reg
            .register(sample(id, "   "))
            .await
            .expect_err("empty name rejected");
        assert!(matches!(err, FamilyClawError::InvalidInput(_)));
        assert!(reg.is_empty().await);
    }

    #[tokio::test]
    async fn deregister_removes_and_returns() {
        let reg = AgentRegistry::new();
        let id = AgentId::new();
        reg.register(sample(id, "agent_a")).await.expect("register");

        let removed = reg.deregister(id).await;
        assert_eq!(removed.map(|i| i.display_name), Some("agent_a".to_string()));
        assert!(!reg.contains(id).await);
        assert!(reg.deregister(id).await.is_none());
    }

    #[tokio::test]
    async fn list_is_sorted_by_id() {
        let reg = AgentRegistry::new();
        let lo = AgentId::from_uuid(uuid::Uuid::from_u128(1));
        let hi = AgentId::from_uuid(uuid::Uuid::from_u128(2));
        // Rekisteröi käänteisessä järjestyksessä.
        reg.register(sample(hi, "agent_hi")).await.expect("reg hi");
        reg.register(sample(lo, "agent_lo")).await.expect("reg lo");

        let list = reg.list().await;
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, lo);
        assert_eq!(list[1].id, hi);
    }

    #[tokio::test]
    async fn heartbeat_unknown_agent_errors() {
        let reg = AgentRegistry::new();
        let err = reg
            .heartbeat(AgentId::new(), ts(1))
            .await
            .expect_err("unknown agent");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn heartbeat_updates_liveness() {
        let reg = AgentRegistry::with_timeout(Duration::seconds(30));
        let id = AgentId::new();
        reg.register(sample(id, "agent_a")).await.expect("register");

        // Ennen heartbeatia → Unknown.
        assert_eq!(
            reg.liveness_at(id, ts(100)).await.expect("liveness"),
            Liveness::Unknown
        );

        reg.heartbeat(id, ts(100)).await.expect("heartbeat");

        // Tuore → Online.
        assert_eq!(
            reg.liveness_at(id, ts(120)).await.expect("liveness"),
            Liveness::Online
        );
        // Vanhentunut → Offline.
        assert_eq!(
            reg.liveness_at(id, ts(200)).await.expect("liveness"),
            Liveness::Offline
        );
    }

    #[tokio::test]
    async fn liveness_unknown_agent_errors() {
        let reg = AgentRegistry::new();
        let err = reg
            .liveness_at(AgentId::new(), ts(1))
            .await
            .expect_err("unknown agent");
        assert!(matches!(err, FamilyClawError::NotFound(_)));
    }

    #[tokio::test]
    async fn online_at_filters_correctly() {
        let reg = AgentRegistry::with_timeout(Duration::seconds(30));
        let a = AgentId::from_uuid(uuid::Uuid::from_u128(1));
        let b = AgentId::from_uuid(uuid::Uuid::from_u128(2));
        let c = AgentId::from_uuid(uuid::Uuid::from_u128(3));
        reg.register(sample(a, "a")).await.expect("reg a");
        reg.register(sample(b, "b")).await.expect("reg b");
        reg.register(sample(c, "c")).await.expect("reg c");

        reg.heartbeat(a, ts(100)).await.expect("hb a"); // online @120
        reg.heartbeat(b, ts(50)).await.expect("hb b"); // offline @120 (70s old)
        // c: ei heartbeatia → Unknown, ei online.

        let online = reg.online_at(ts(120)).await;
        assert_eq!(online.len(), 1);
        assert_eq!(online[0].id, a);
    }

    #[tokio::test]
    async fn negative_timeout_is_normalized_to_zero() {
        let reg = AgentRegistry::with_timeout(Duration::seconds(-5));
        assert_eq!(reg.heartbeat_timeout(), Duration::zero());
        let id = AgentId::new();
        reg.register(sample(id, "agent_a")).await.expect("register");
        reg.heartbeat(id, ts(100)).await.expect("heartbeat");
        // Tasan nyt → Online (<= 0).
        assert_eq!(
            reg.liveness_at(id, ts(100)).await.expect("liveness"),
            Liveness::Online
        );
        // 1 s myöhemmin → Offline.
        assert_eq!(
            reg.liveness_at(id, ts(101)).await.expect("liveness"),
            Liveness::Offline
        );
    }

    #[tokio::test]
    async fn registry_clone_shares_state() {
        let reg = AgentRegistry::new();
        let clone = reg.clone();
        let id = AgentId::new();
        reg.register(sample(id, "agent_a")).await.expect("register");
        // Klooni näkee saman tilan (jaettu Arc).
        assert!(clone.contains(id).await);
    }
}
