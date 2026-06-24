//! MCP-sovitin: kuvaa toimintopinon taidot MCP-työkaluiksi ja reitittää
//! työkalukutsut capability-tarkistuksen läpi (KERROS A).
//!
//! Tämä moduuli on tarkoituksella **rajapinta**, ei täysi MCP-palvelin: se
//! määrittelee miten taidot esitetään MCP-työkaluina ([`McpToolDescriptor`]),
//! miten työkalua kutsutaan ([`McpToolCall`]) ja mitä se palauttaa
//! ([`McpToolResult`]), sekä käytäntöportin ([`call_with_policy`]) joka:
//! - hylkää tuntemattoman työkalun ([`ActionError::McpUnknownTool`]),
//! - hylkää kutsun jos vaadittu oikeus puuttuu myönnetyistä
//!   ([`ActionError::McpDenied`]) ja kirjaa eväyksen audit-lokiin,
//! - merkitsee tulosteen epäluotettavaksi (taint) **ellei** työkalun lähde ole
//!   eksplisiittisesti luotettu ([`McpToolDescriptor::trusted`]).
//!
//! ## OSS-raja (KERROS A)
//! Tarjoajat ovat **mockeja** ([`MockMcpProvider`]) — ei oikeita verkkokutsuja,
//! ei providereita, sieluja eikä avaimia. Tuloste on oletuksena epäluotettava,
//! kuten suorituskerroksessakin ([`crate::executor`]), kunnes lähde on todettu
//! luotettavaksi.
//!
//! ## Determinismi
//! Käytäntöportti ottaa aikaleiman injektoituna
//! ([`familyclaw_core::time::Timestamp`]) — kelloa ei lueta logiikan sisällä,
//! jotta audit-tapahtumat ovat deterministisiä testeissä ja replayssa.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use familyclaw_core::time::Timestamp;

use crate::audit::{AuditCollector, AuditKind, ExecAuditEvent};
use crate::error::{ActionError, Result};
use crate::ids::ActionId;
use crate::policy::SkillPermission;

/// Moduulin valmiusaste — säilytetään, jotta [`crate::all_modules_scaffolded`]
/// kääntyy edelleen muiden moduulien rinnalla.
pub(crate) const SCAFFOLDED: bool = true;

/// Yhden MCP-työkalun kuvaus: se mitä tarjoaja julkaisee asiakkaalle.
///
/// Kuvaus kertoo työkalun nimen ja kuvauksen, sen syöteskeeman (geneerinen
/// JSON-schema arvona), työkalun vaatiman oikeuden sekä onko työkalun lähde
/// luotettu. Luotettu lähde tuottaa luotettua dataa; muutoin tuloste
/// merkitään epäluotettavaksi (taint).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolDescriptor {
    /// Työkalun yksilöivä nimi (esim. `echo`). Käytetään reitityksessä.
    pub name: String,
    /// Lyhyt ihmisluettava kuvaus mitä työkalu tekee.
    pub description: String,
    /// Työkalun syöteskeema geneerisenä JSON-arvona (esim. JSON-schema).
    pub input_schema: Value,
    /// Oikeus jonka kutsujalla on oltava ennen kuin työkalua saa kutsua.
    pub required_permission: SkillPermission,
    /// Onko työkalun lähde luotettu. Jos `true`, tuloste ei saa taint-leimaa;
    /// jos `false`, tuloste merkitään epäluotettavaksi.
    pub trusted: bool,
}

impl McpToolDescriptor {
    /// Rakentaa uuden työkalukuvauksen.
    ///
    /// Lähde merkitään oletuksena **epäluotetuksi** (`trusted = false`);
    /// luotettavuus on nostettava eksplisiittisesti [`McpToolDescriptor::trust`]:lla.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        required_permission: SkillPermission,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            required_permission,
            trusted: false,
        }
    }

    /// Merkitsee työkalun lähteen luotetuksi (tuloste ei saa taint-leimaa).
    ///
    /// Käytetään vain kun lähde on eksplisiittisesti todettu luotettavaksi.
    #[must_use]
    pub fn trust(mut self) -> Self {
        self.trusted = true;
        self
    }
}

/// Yhden MCP-työkalukutsun pyyntö: mihin työkaluun ja millä syötteellä.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolCall {
    /// Kutsuttavan työkalun nimi (täsmättävä [`McpToolDescriptor::name`]-arvoon).
    pub tool: String,
    /// Työkalulle välitettävä syöte geneerisenä JSON-arvona.
    pub input: Value,
}

impl McpToolCall {
    /// Rakentaa uuden työkalukutsun.
    #[must_use]
    pub fn new(tool: impl Into<String>, input: Value) -> Self {
        Self {
            tool: tool.into(),
            input,
        }
    }
}

/// Yhden MCP-työkalukutsun tulos.
///
/// `untrusted` kertoo onko tuloste peräisin epäluotettavasta lähteestä (taint).
/// Tarjoajan oma tulos on oletuksena epäluotettava; käytäntöportti
/// ([`call_with_policy`]) nollaa leiman vain jos työkalukuvauksen lähde on
/// luotettu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpToolResult {
    /// Työkalun tuottama tuloste geneerisenä JSON-arvona.
    pub output: Value,
    /// Onko tuloste peräisin epäluotettavasta lähteestä (taint).
    pub untrusted: bool,
}

impl McpToolResult {
    /// Rakentaa uuden epäluotettavaksi merkityn tuloksen (oletus taint).
    #[must_use]
    pub fn untrusted(output: Value) -> Self {
        Self {
            output,
            untrusted: true,
        }
    }

    /// Rakentaa uuden luotetuksi merkityn tuloksen (ei taint-leimaa).
    #[must_use]
    pub fn trusted(output: Value) -> Self {
        Self {
            output,
            untrusted: false,
        }
    }
}

/// MCP-työkalujen tarjoaja.
///
/// Toteutus julkaisee joukon työkaluja ([`McpToolProvider::describe`]) ja ajaa
/// yksittäisen työkalukutsun ([`McpToolProvider::call`]). KERROS A -toteutukset
/// ovat **mockeja** — ei oikeita verkkokutsuja.
#[async_trait]
pub trait McpToolProvider: Send + Sync {
    /// Palauttaa kaikki tarjoajan julkaisemat työkalukuvaukset.
    async fn describe(&self) -> Vec<McpToolDescriptor>;

    /// Ajaa yhden työkalukutsun ja palauttaa tuloksen.
    ///
    /// # Errors
    /// Palauttaa [`ActionError::McpUnknownTool`] jos työkalua ei ole, ja muita
    /// [`ActionError`]-variantteja jos suoritus ei voi alkaa. Suositeltavaa on
    /// reitittää kutsut [`call_with_policy`]:n kautta, joka tekee oikeus- ja
    /// taint-tarkistukset.
    async fn call(&self, call: McpToolCall) -> Result<McpToolResult>;
}

/// Yhden mock-työkalun toiminta: kuvaus + valmis tuloste.
#[derive(Debug, Clone)]
struct MockTool {
    /// Työkalun kuvaus jonka tarjoaja julkaisee.
    descriptor: McpToolDescriptor,
    /// Kiinteä tuloste jonka työkalu palauttaa (mock — ei verkkokutsua).
    /// `None` tarkoittaa "kaiuta syöte takaisin" (esim. `echo`-työkalu).
    canned: Option<Value>,
}

/// Testikäyttöinen MCP-tarjoaja, jolla on muistinvarainen työkalurekisteri.
///
/// Oletuksena rekisteri sisältää kaksi geneeristä mock-työkalua:
/// - `echo` — kaiuttaa syötteen takaisin tulosteena (epäluotettu lähde),
/// - `fetch_mock` — palauttaa kiinteän valmiin tuloksen (epäluotettu lähde).
///
/// Lisää työkaluja voi rekisteröidä [`MockMcpProvider::with_tool`]:lla.
/// Yksikään mock ei tee verkkokutsuja (KERROS A).
#[derive(Debug, Clone, Default)]
pub struct MockMcpProvider {
    /// Työkalut nimellä avainnettuna (vakaa, deterministinen järjestys).
    tools: BTreeMap<String, MockTool>,
}

impl MockMcpProvider {
    /// Luo tyhjän tarjoajan ilman työkaluja.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Luo tarjoajan oletustyökaluilla (`echo`, `fetch_mock`).
    ///
    /// Oletustyökalut vaativat [`SkillPermission::NetworkRead`]-oikeuden ja
    /// niiden lähde on epäluotettu (tuloste taintataan ellei kutsuja erikseen
    /// merkitse työkalua luotetuksi).
    #[must_use]
    pub fn with_defaults() -> Self {
        let echo = McpToolDescriptor::new(
            "echo",
            "Kaiuttaa syötteen takaisin sellaisenaan.",
            serde_json::json!({ "type": "object" }),
            SkillPermission::NetworkRead,
        );
        let fetch = McpToolDescriptor::new(
            "fetch_mock",
            "Palauttaa kiinteän valmiin tuloksen (mock-haku).",
            serde_json::json!({ "type": "object" }),
            SkillPermission::NetworkRead,
        );
        Self::empty().with_tool(echo, None).with_tool(
            fetch,
            Some(serde_json::json!({ "status": "ok", "items": [] })),
        )
    }

    /// Rekisteröi työkalun kuvauksella ja valmiilla tuloksella.
    ///
    /// Jos `canned` on `None`, työkalu kaiuttaa kutsun syötteen takaisin
    /// tulosteena. Sama nimi korvaa aiemman rekisteröinnin.
    #[must_use]
    pub fn with_tool(mut self, descriptor: McpToolDescriptor, canned: Option<Value>) -> Self {
        let name = descriptor.name.clone();
        self.tools.insert(name, MockTool { descriptor, canned });
        self
    }

    /// Hakee työkalun kuvauksen nimellä (jos rekisteröity).
    #[must_use]
    pub fn descriptor(&self, name: &str) -> Option<&McpToolDescriptor> {
        self.tools.get(name).map(|t| &t.descriptor)
    }
}

#[async_trait]
impl McpToolProvider for MockMcpProvider {
    async fn describe(&self) -> Vec<McpToolDescriptor> {
        self.tools.values().map(|t| t.descriptor.clone()).collect()
    }

    async fn call(&self, call: McpToolCall) -> Result<McpToolResult> {
        let Some(tool) = self.tools.get(&call.tool) else {
            return Err(ActionError::McpUnknownTool(call.tool));
        };
        // Mock: joko kaiuta syöte tai palauta valmis tulos. Aina epäluotettu
        // lähteenä; käytäntöportti päättää lopullisen taint-tilan kuvauksen
        // `trusted`-lipun perusteella.
        let output = tool.canned.clone().unwrap_or(call.input);
        Ok(McpToolResult::untrusted(output))
    }
}

/// Reitittää työkalukutsun käytäntöportin läpi: oikeustarkistus, audit-kirjaus
/// ja taint-merkintä.
///
/// Vaiheet:
/// 1. **Tuntematon työkalu** → [`ActionError::McpUnknownTool`] (ei audit-
///    kirjausta: kutsua ei ollut olemassa).
/// 2. **Oikeus puuttuu** → [`ActionError::McpDenied`] ja
///    [`AuditKind::PolicyDenied`]-tapahtuma kirjataan.
/// 3. **Sallittu** → tarjoaja ajaa kutsun. Tuloste merkitään
///    epäluotettavaksi ([`AuditKind::TaintMarked`]) **ellei** kuvauksen lähde
///    ole luotettu; luotetulla lähteellä leima nollataan.
///
/// `action_id` sitoo audit-tapahtumat tähän kutsuun, `now` on injektoitu
/// aikaleima (ei luettu kellosta), `audit` kerää tapahtumat.
///
/// # Errors
/// Palauttaa [`ActionError::McpUnknownTool`] tuntemattomalle työkalulle,
/// [`ActionError::McpDenied`] kun vaadittu oikeus puuttuu, ja edelleen
/// tarjoajan palauttaman virheen jos suoritus epäonnistuu.
pub async fn call_with_policy<P: McpToolProvider + ?Sized>(
    provider: &P,
    granted_permissions: &[SkillPermission],
    call: McpToolCall,
    now: Timestamp,
    audit: &AuditCollector,
    action_id: ActionId,
) -> Result<McpToolResult> {
    // 1. Etsi työkalukuvaus. Tuntematon työkalu hylätään ennen mitään muuta.
    let descriptors = provider.describe().await;
    let Some(descriptor) = descriptors.into_iter().find(|d| d.name == call.tool) else {
        return Err(ActionError::McpUnknownTool(call.tool));
    };

    // 2. Oikeustarkistus: vaadittu oikeus on oltava myönnettyjen joukossa.
    if !granted_permissions.contains(&descriptor.required_permission) {
        audit.record(ExecAuditEvent::new(
            AuditKind::PolicyDenied,
            action_id,
            now,
            format!(
                "mcp tool '{}' denied: missing required permission",
                descriptor.name
            ),
        ));
        return Err(ActionError::McpDenied(format!(
            "tool '{}' requires a permission not in the granted set",
            descriptor.name
        )));
    }

    // 3. Sallittu — aja kutsu tarjoajalla.
    let result = provider.call(call).await?;

    // 4. Taint-päätös: luotettu lähde nollaa leiman, muutoin tuloste taintataan.
    if descriptor.trusted {
        Ok(McpToolResult::trusted(result.output))
    } else {
        audit.record(ExecAuditEvent::new(
            AuditKind::TaintMarked,
            action_id,
            now,
            format!("mcp tool '{}' output marked untrusted", descriptor.name),
        ));
        Ok(McpToolResult::untrusted(result.output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::time::from_unix_secs;
    use serde_json::json;

    /// Apuri: injektoitu aikaleima testeihin.
    fn ts() -> Timestamp {
        from_unix_secs(1_700_000_000).expect("valid unix seconds")
    }

    #[tokio::test]
    async fn defaults_register_echo_and_fetch() {
        let provider = MockMcpProvider::with_defaults();
        let described = provider.describe().await;
        let names: Vec<&str> = described.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"echo"));
        assert!(names.contains(&"fetch_mock"));
        // Oletustyökalut ovat epäluotettuja lähteinä.
        assert!(described.iter().all(|d| !d.trusted));
    }

    #[tokio::test]
    async fn registered_tool_callable_through_policy_ok_and_untrusted_by_default() {
        let provider = MockMcpProvider::with_defaults();
        let audit = AuditCollector::new();
        let action_id = ActionId::new();
        let granted = [SkillPermission::NetworkRead];

        let call = McpToolCall::new("echo", json!({ "user": "agent_a", "msg": "hi" }));
        let result = call_with_policy(&provider, &granted, call, ts(), &audit, action_id)
            .await
            .expect("granted permission allows call");

        // echo kaiuttaa syötteen takaisin.
        assert_eq!(result.output, json!({ "user": "agent_a", "msg": "hi" }));
        // Oletuksena epäluotettu (taint), koska lähde ei ole luotettu.
        assert!(result.untrusted);
        // Taint-merkintä kirjattiin audit-lokiin.
        assert!(audit
            .list()
            .iter()
            .any(|e| e.kind == AuditKind::TaintMarked && e.action_id == action_id));
    }

    #[tokio::test]
    async fn fetch_mock_returns_canned_result() {
        let provider = MockMcpProvider::with_defaults();
        let audit = AuditCollector::new();
        let granted = [SkillPermission::NetworkRead];

        let call = McpToolCall::new("fetch_mock", json!({ "query": "general" }));
        let result = call_with_policy(&provider, &granted, call, ts(), &audit, ActionId::new())
            .await
            .expect("fetch_mock allowed");
        assert_eq!(result.output, json!({ "status": "ok", "items": [] }));
        assert!(result.untrusted);
    }

    #[tokio::test]
    async fn unknown_tool_rejected() {
        let provider = MockMcpProvider::with_defaults();
        let audit = AuditCollector::new();
        let granted = [SkillPermission::NetworkRead];

        let call = McpToolCall::new("does_not_exist", json!({}));
        let err = call_with_policy(&provider, &granted, call, ts(), &audit, ActionId::new())
            .await
            .expect_err("unknown tool must be rejected");
        assert!(matches!(err, ActionError::McpUnknownTool(_)));
        // Tuntemattomasta työkalusta ei synny audit-tapahtumaa.
        assert!(audit.is_empty());
    }

    #[tokio::test]
    async fn denied_permission_blocks_call_and_records_audit() {
        let provider = MockMcpProvider::with_defaults();
        let audit = AuditCollector::new();
        let action_id = ActionId::new();
        // Myönnetään VÄÄRÄ oikeus (työkalu vaatii NetworkRead).
        let granted = [SkillPermission::ReadFiles];

        let call = McpToolCall::new("echo", json!({ "user": "agent_a" }));
        let err = call_with_policy(&provider, &granted, call, ts(), &audit, action_id)
            .await
            .expect_err("missing permission must block");
        assert!(matches!(err, ActionError::McpDenied(_)));

        // Eväys kirjattiin audit-lokiin.
        let events = audit.list();
        assert!(events
            .iter()
            .any(|e| e.kind == AuditKind::PolicyDenied && e.action_id == action_id));
        // Eikä taint-tapahtumaa synny kun kutsu estettiin.
        assert!(!events.iter().any(|e| e.kind == AuditKind::TaintMarked));
    }

    #[tokio::test]
    async fn trusted_source_output_is_not_tainted() {
        // Rekisteröi luotettu työkalu kiinteällä tuloksella.
        let trusted_tool = McpToolDescriptor::new(
            "trusted_lookup",
            "Luotettu sisäinen haku.",
            json!({ "type": "object" }),
            SkillPermission::NetworkRead,
        )
        .trust();
        let provider =
            MockMcpProvider::empty().with_tool(trusted_tool, Some(json!({ "result": "general" })));
        let audit = AuditCollector::new();
        let granted = [SkillPermission::NetworkRead];

        let call = McpToolCall::new("trusted_lookup", json!({ "q": "x" }));
        let result = call_with_policy(&provider, &granted, call, ts(), &audit, ActionId::new())
            .await
            .expect("trusted tool allowed");

        // Luotettu lähde → ei taint-leimaa.
        assert!(!result.untrusted);
        assert_eq!(result.output, json!({ "result": "general" }));
        // Eikä taint-tapahtumaa kirjata luotetulle lähteelle.
        assert!(!audit
            .list()
            .iter()
            .any(|e| e.kind == AuditKind::TaintMarked));
    }

    #[tokio::test]
    async fn secret_looking_output_passes_through_call_result_for_proof_redaction() {
        // Tarjoaja ei redaktoi itse — redaktointi tapahtuu todistepaketissa.
        // Tässä varmistetaan vain että salaisuudelta näyttävä arvo kulkee
        // tuloksessa läpi ilman lähdeliteraalia (Layer B -audit).
        let fake = format!("sk-{}", "live".repeat(4));
        let tool = McpToolDescriptor::new(
            "leaky_mock",
            "Palauttaa salaisuudelta näyttävän arvon (taintataan).",
            json!({ "type": "object" }),
            SkillPermission::NetworkRead,
        );
        let provider =
            MockMcpProvider::empty().with_tool(tool, Some(json!({ "blob": fake.clone() })));
        let audit = AuditCollector::new();
        let granted = [SkillPermission::NetworkRead];

        let call = McpToolCall::new("leaky_mock", json!({}));
        let result = call_with_policy(&provider, &granted, call, ts(), &audit, ActionId::new())
            .await
            .expect("call allowed");
        // Epäluotettu lähde → taint asetettu (redaktointi tehdään proof-kerroksessa).
        assert!(result.untrusted);
        assert_eq!(result.output, json!({ "blob": fake }));
    }

    #[tokio::test]
    async fn provider_call_directly_rejects_unknown_tool() {
        let provider = MockMcpProvider::with_defaults();
        let err = provider
            .call(McpToolCall::new("nope", json!({})))
            .await
            .expect_err("unknown tool rejected at provider level");
        assert!(matches!(err, ActionError::McpUnknownTool(_)));
    }

    #[test]
    fn descriptor_and_call_serde_roundtrip() {
        let desc = McpToolDescriptor::new(
            "echo",
            "Kaiuttaa.",
            json!({ "type": "object" }),
            SkillPermission::NetworkRead,
        );
        let json_str = serde_json::to_string(&desc).expect("serialize descriptor");
        let back: McpToolDescriptor = serde_json::from_str(&json_str).expect("deserialize");
        assert_eq!(desc, back);

        let call = McpToolCall::new("echo", json!({ "x": 1 }));
        let back_call: McpToolCall =
            serde_json::from_str(&serde_json::to_string(&call).expect("ser")).expect("de");
        assert_eq!(call, back_call);
    }
}
