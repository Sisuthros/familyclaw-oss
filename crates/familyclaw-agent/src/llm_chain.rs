//! Config → runtime -silta: [`ModelConfig`] → ajettava [`LlmFailover`].
//!
//! Tämä moduuli täyttää suunnittelun aukon (recon: *ei* `build_llm_chain`):
//! konfiguraatiokerroksen [`ModelConfig`]`{primary, fallbacks}`
//! (`familyclaw-core`) muunnetaan järjestetyksi ketjuksi ajettavia
//! [`LlmConfig`]-asetuksia (`crate::llm`). Itse mallinimi → endpoint/avain
//! -kuvaus on **resolverin** vastuulla ([`LlmEndpointResolver`]), jotta
//! KERROS A (tämä OSS-runko) ei kovakoodaa endpointteja, avaimia eikä
//! provider-nimiä.
//!
//! ## Kerrosraja (KERROS A / KERROS B)
//! - **KERROS A (tämä tiedosto):** trait-raja + ketjun rakennus + failover.
//!   Ei avaimia, ei endpointteja, ei perheenjäsenten malleja.
//! - **KERROS B (esim. [`EnvEndpointResolver`]):** kuvaa `"provider/model"`
//!   -merkkijonon ajettavaksi [`LlmConfig`]:ksi lukien API-avaimet
//!   ympäristömuuttujista (esim. `OPENCODE_API_KEY`, `DEEPSEEK_API_KEY`).
//!   [`EnvEndpointResolver`] on geneerinen apuri — se ei tunne perhettä,
//!   vain provider-prefiksin.
//!
//! ## Esimerkki
//! ```
//! use familyclaw_agent::llm_chain::{build_llm_chain, EnvEndpointResolver};
//! use familyclaw_core::ModelConfig;
//!
//! // Provider-prefiksit kuvataan endpointteihin; avaimet luetaan env:stä.
//! let resolver = EnvEndpointResolver::new()
//!     .with_provider("openai", "https://api.openai.com/v1", "OPENAI_API_KEY");
//! let model = ModelConfig::new("openai/gpt-4o").with_fallback("openai/gpt-4o-mini");
//!
//! // Avain voi puuttua testiympäristössä → tyhjä avain on sallittu rakennukseen,
//! // virhe näkyy vasta varsinaisessa complete()-kutsussa.
//! let chain = build_llm_chain(&model, &resolver).expect("chain builds");
//! assert_eq!(chain.primary_model(), "openai/gpt-4o");
//! assert_eq!(chain.len(), 2);
//! ```

use std::collections::HashMap;

use familyclaw_core::{FamilyClawError, ModelConfig, Result};

use crate::llm::{LlmClient, LlmConfig, LlmError, LlmMessage};

/// Kuvaa mallinimen (`"provider/model"`) ajettavaksi [`LlmConfig`]:ksi.
///
/// KERROS B toteuttaa tämän (endpointit + avaimet). KERROS A (ketjun
/// rakennus) näkee vain trait-rajan, joten OSS-runko pysyy puhtaana
/// kovakoodatuista endpointeista ja avaimista.
pub trait LlmEndpointResolver: Send + Sync {
    /// Ratkaisee mallinimen ajettavaksi asetukseksi.
    ///
    /// # Errors
    /// [`FamilyClawError::Config`] jos mallinimeä ei voida kuvata
    /// endpointiksi (esim. tuntematon provider-prefiksi).
    fn resolve(&self, model_name: &str) -> Result<LlmConfig>;
}

/// Geneerinen, env-pohjainen resolveri (KERROS B -apuri).
///
/// Kuvaa `"provider/model"`-merkkijonon endpointiksi provider-prefiksin
/// perusteella ja lukee API-avaimen ympäristömuuttujasta. Provider-taulu
/// rekisteröidään ajonaikaisesti — mitään perhe- tai malliriippuvaista
/// tietoa ei kovakoodata tähän.
///
/// Mallinimen muoto: `"<provider>/<model>"`. Esim. `"openai/gpt-4o"` →
/// provider `"openai"`, malli `"gpt-4o"`. Jos `/`-erotinta ei ole, koko
/// merkkijonoa käytetään sekä providerin avaimena että mallinimenä.
#[derive(Debug, Clone, Default)]
pub struct EnvEndpointResolver {
    /// provider-prefiksi → (`api_base`, env-muuttujan nimi).
    providers: HashMap<String, (String, String)>,
    /// Maksimi tokenit per vastaus (välitetään jokaiseen [`LlmConfig`]:iin).
    max_tokens: Option<u32>,
}

impl EnvEndpointResolver {
    /// Rakentaa tyhjän resolverin ilman provider-kuvauksia.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rekisteröi provider-prefiksin: endpoint + env-muuttuja josta avain
    /// luetaan ajonaikaisesti (builder-tyyli).
    ///
    /// - `prefix` — mallinimen `provider/`-osa, esim. `"openai"`.
    /// - `api_base` — OpenAI-yhteensopiva base-URL.
    /// - `key_env` — ympäristömuuttuja, esim. `"OPENAI_API_KEY"`.
    #[must_use]
    pub fn with_provider(
        mut self,
        prefix: impl Into<String>,
        api_base: impl Into<String>,
        key_env: impl Into<String>,
    ) -> Self {
        self.providers
            .insert(prefix.into(), (api_base.into(), key_env.into()));
        self
    }

    /// Asettaa max_tokens-arvon kaikkiin ratkaistuihin asetuksiin.
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Pilkkoo mallinimen `(provider, malli)`-pariksi.
    fn split(model_name: &str) -> (&str, &str) {
        match model_name.split_once('/') {
            Some((provider, model)) => (provider, model),
            None => (model_name, model_name),
        }
    }
}

impl LlmEndpointResolver for EnvEndpointResolver {
    fn resolve(&self, model_name: &str) -> Result<LlmConfig> {
        let (provider, model) = Self::split(model_name);
        let (api_base, key_env) = self.providers.get(provider).ok_or_else(|| {
            FamilyClawError::config(format!("unknown provider prefix for model '{model_name}'"))
        })?;
        // Avain luetaan ajonaikaisesti env:stä. Puuttuva avain ei estä
        // ketjun rakennusta (esim. fallback-mallit, joita ei kenties tarvita)
        // — tyhjä avain päätyy LlmConfigiin ja virhe näkyy vasta complete():ssa.
        let api_key = std::env::var(key_env).unwrap_or_default();
        let mut cfg = LlmConfig::new(api_base.clone(), api_key, model.to_string());
        if let Some(max) = self.max_tokens {
            cfg = cfg.with_max_tokens(max);
        }
        Ok(cfg)
    }
}

/// Järjestetty failover-ketju ajettavia LLM-klientteja.
///
/// Rakennetaan [`ModelConfig`]:sta [`build_llm_chain`]illa: ensin `primary`,
/// sitten `fallbacks` järjestyksessä ([`ModelConfig::preference_order`]).
/// [`complete`](LlmFailover::complete) yrittää jokaista klienttiä järjestyksessä
/// kunnes yksi onnistuu.
pub struct LlmFailover {
    chain: Vec<LlmClient>,
    /// Primary-mallin nimi (`preference_order`in ensimmäinen), raportointiin.
    primary: String,
}

impl LlmFailover {
    /// Yrittää `complete()`:ä jokaisella klientilla järjestyksessä kunnes yksi
    /// onnistuu. Palauttaa viimeisen virheen jos kaikki epäonnistuvat.
    ///
    /// # Errors
    /// Viimeisin [`LlmError`] jos kaikki ketjun klientit epäonnistuvat, tai
    /// [`LlmError::NoContent`] jos ketju on tyhjä.
    pub async fn complete(&self, messages: &[LlmMessage]) -> std::result::Result<String, LlmError> {
        let mut last_err: Option<LlmError> = None;
        for client in &self.chain {
            match client.complete(messages).await {
                Ok(text) => return Ok(text),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or(LlmError::NoContent))
    }

    /// Primary-mallin nimi (`preference_order`in ensimmäinen).
    #[must_use]
    pub fn primary_model(&self) -> &str {
        &self.primary
    }

    /// Ketjun pituus (primary + onnistuneesti ratkaistut fallbackit).
    #[must_use]
    pub fn len(&self) -> usize {
        self.chain.len()
    }

    /// Onko ketju tyhjä.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chain.is_empty()
    }

    /// Primary-klientin ajettava asetus (luku). `None` jos ketju on tyhjä.
    #[must_use]
    pub fn primary_config(&self) -> Option<&LlmConfig> {
        self.chain.first().map(LlmClient::config)
    }
}

/// Rakentaa failover-ketjun [`ModelConfig`]:sta resolverin avulla.
///
/// Iteroi [`ModelConfig::preference_order`]in (primary → fallbacks) ja
/// ratkaisee jokaisen mallinimen [`LlmConfig`]:ksi resolverilla. Mallit, joita
/// resolveri ei tunne, **ohitetaan** (eivät kaada koko ketjua) — näin yksi
/// kelvoton fallback ei estä toimivaa primaryä.
///
/// # Errors
/// [`FamilyClawError::Config`] jos `primary` on tyhjä tai jos **yksikään**
/// malli `preference_order`issa ei ratkennut (tyhjä ketju on kelvoton).
pub fn build_llm_chain(
    cfg: &ModelConfig,
    resolver: &dyn LlmEndpointResolver,
) -> Result<LlmFailover> {
    cfg.validate()?;
    let primary = cfg.primary.clone();
    let mut chain = Vec::new();
    for model_name in cfg.preference_order() {
        match resolver.resolve(model_name) {
            Ok(llm_cfg) => chain.push(LlmClient::new(llm_cfg)),
            Err(e) => {
                // Ohita tuntematon malli mutta kirjaa syy debug-tasolla.
                tracing::debug!(model = model_name, error = %e, "skipping unresolvable model");
            }
        }
    }
    if chain.is_empty() {
        return Err(FamilyClawError::config(format!(
            "no usable model: none of '{}' (+{} fallbacks) resolved to an endpoint",
            cfg.primary,
            cfg.fallbacks.len()
        )));
    }
    Ok(LlmFailover { chain, primary })
}

/// Poimii agentin primary-[`LlmConfig`]:n config-kerroksesta — valmis
/// syötettäväksi [`Agent::new`](crate::Agent::new):lle (joka ottaa
/// `Option<LlmConfig>`).
///
/// Tämä on kevyt silta TEHTÄVÄ C4:lle: `FamilyConfig` →
/// (agentti, [`ModelConfig`]) → ajettava primary-asetus. Agentin
/// julkista konstruktiopintaa ei muuteta — palautetaan vain valmis
/// `Option<LlmConfig>`, jonka kutsuja antaa eteenpäin.
///
/// # Errors
/// [`FamilyClawError::Config`] jos mallikonfiguraatio on kelvoton tai jos
/// yksikään malli ei ratkennut endpointiksi.
pub fn primary_llm_config(
    model: &ModelConfig,
    resolver: &dyn LlmEndpointResolver,
) -> Result<LlmConfig> {
    let chain = build_llm_chain(model, resolver)?;
    chain
        .primary_config()
        .cloned()
        .ok_or_else(|| FamilyClawError::config("empty llm chain has no primary config"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_core::{AgentConfig, FamilyConfig};

    /// Resolveri joka tuntee provider-prefiksit ilman env-riippuvuutta.
    fn test_resolver() -> EnvEndpointResolver {
        EnvEndpointResolver::new()
            .with_provider("openai", "https://api.openai.com/v1", "OPENAI_API_KEY")
            .with_provider("deepseek", "https://api.deepseek.com/v1", "DEEPSEEK_API_KEY")
            .with_provider("opencode", "https://opencode.ai/zen/v1", "OPENCODE_API_KEY")
    }

    #[test]
    fn split_handles_provider_prefix_and_bare_name() {
        assert_eq!(
            EnvEndpointResolver::split("openai/gpt-4o"),
            ("openai", "gpt-4o")
        );
        assert_eq!(EnvEndpointResolver::split("bare-model"), ("bare-model", "bare-model"));
    }

    #[test]
    fn resolver_maps_provider_to_endpoint() {
        let r = test_resolver();
        let cfg = r.resolve("deepseek/deepseek-v4-pro").expect("resolves");
        assert_eq!(cfg.api_base, "https://api.deepseek.com/v1");
        assert_eq!(cfg.model, "deepseek-v4-pro");
    }

    #[test]
    fn resolver_rejects_unknown_provider() {
        let r = test_resolver();
        let err = r.resolve("mystery/model").expect_err("unknown provider rejected");
        assert!(matches!(err, FamilyClawError::Config(_)));
    }

    #[test]
    fn build_chain_orders_primary_then_fallbacks() {
        let r = test_resolver();
        let model = ModelConfig::new("openai/gpt-4o")
            .with_fallback("deepseek/deepseek-v4-pro")
            .with_fallback("opencode/big-pickle");
        let chain = build_llm_chain(&model, &r).expect("chain builds");
        assert_eq!(chain.len(), 3);
        assert_eq!(chain.primary_model(), "openai/gpt-4o");
        assert_eq!(
            chain.primary_config().expect("primary").api_base,
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn build_chain_skips_unresolvable_fallback_but_keeps_primary() {
        let r = test_resolver();
        let model = ModelConfig::new("openai/gpt-4o").with_fallback("mystery/model");
        let chain = build_llm_chain(&model, &r).expect("primary still usable");
        assert_eq!(chain.len(), 1, "unresolvable fallback dropped");
        assert_eq!(chain.primary_model(), "openai/gpt-4o");
    }

    #[test]
    fn build_chain_errors_when_nothing_resolves() {
        let r = test_resolver();
        let model = ModelConfig::new("mystery/a").with_fallback("mystery/b");
        // Result<LlmFailover>: LlmFailover ei toteuta Debugia (LlmClient ei),
        // joten matchataan suoraan ilman expect_err:iä.
        match build_llm_chain(&model, &r) {
            Err(FamilyClawError::Config(_)) => {}
            Err(other) => panic!("expected Config error, got {other:?}"),
            Ok(_) => panic!("expected error for empty chain"),
        }
    }

    #[test]
    fn build_chain_errors_on_invalid_model_config() {
        let r = test_resolver();
        let model = ModelConfig::new("   ");
        assert!(build_llm_chain(&model, &r).is_err());
    }

    #[test]
    fn primary_llm_config_returns_ready_config() {
        let r = test_resolver();
        let model = ModelConfig::new("deepseek/deepseek-v4-pro");
        let cfg = primary_llm_config(&model, &r).expect("primary config");
        assert_eq!(cfg.model, "deepseek-v4-pro");
    }

    /// TEHTÄVÄ C4 -hyväksyntä: FamilyConfig-JSON → agentti rakentuu ilman
    /// paniikkia (primary LlmConfig saadaan config-kerroksesta + resolverista).
    #[test]
    fn family_json_builds_agent_llm_config_without_panic() {
        let json = r#"{
            "name": "demo_family",
            "agents": [
                {
                    "name": "agent_a",
                    "model": {
                        "primary": "deepseek/deepseek-v4-pro",
                        "fallbacks": ["openai/gpt-4o", "mystery/skip-me"]
                    }
                }
            ]
        }"#;
        let family = FamilyConfig::from_json_str(json).expect("config parses + validates");
        let resolver = test_resolver();

        let agent: &AgentConfig = family.agents.first().expect("one agent");
        let chain = build_llm_chain(&agent.model, &resolver).expect("chain builds");
        // primary + yksi tunnettu fallback; tuntematon "mystery/" ohitettu.
        assert_eq!(chain.len(), 2);
        assert_eq!(chain.primary_model(), "deepseek/deepseek-v4-pro");

        // Valmis primary-asetus, jonka kutsuja antaa Agent::new(Some(cfg)):lle.
        let primary = primary_llm_config(&agent.model, &resolver).expect("primary config");
        assert_eq!(primary.model, "deepseek-v4-pro");
    }

    #[tokio::test]
    async fn complete_on_empty_chain_path_is_no_content() {
        // Suora rakennus tyhjällä ketjulla ei ole sallittu rajapinnan kautta,
        // mutta complete()-semantiikka tyhjälle ketjulle on määritelty:
        // varmistetaan ettei se paniikkaa.
        let failover = LlmFailover {
            chain: Vec::new(),
            primary: String::new(),
        };
        assert!(failover.is_empty());
        let err = failover
            .complete(&[LlmMessage::user("hi")])
            .await
            .expect_err("empty chain yields error, not panic");
        assert!(matches!(err, LlmError::NoContent));
    }
}
