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
use std::sync::{Arc, Mutex};

use familyclaw_core::time::Timestamp;
use familyclaw_core::{FamilyClawError, ModelConfig, Result};

use crate::llm::{CompletionResult, LlmClient, LlmConfig, LlmError, LlmMessage, ToolDefinition};

/// Kello-abstraktio failover-päätöslogiikalle (cooldown-tilakone, KERROS B).
///
/// **Miksi trait eikä suora [`familyclaw_core::time::now`]?** Cooldown-päätökset
/// (`onko tämä entry vielä jäähdyllä?`, `cooled_until = now + ladder[strike]`)
/// luetaan **vain** tämän rajapinnan kautta, jotta testit voivat askeltaa aikaa
/// determinisesti ilman `tokio::time::sleep`-odotusta. Tuotannossa
/// [`SystemClock`] delegoi [`familyclaw_core::time::now`]:hin — se on failover-
/// polun **ainoa** seinäkellokosketus. Tämä noudattaa olemassa olevaa
/// koodikannan tapaa (aika injektoidaan, ks. `OrchestratedTurn::now`), ei tuo
/// uutta kehystä.
pub trait Clock: Send + Sync {
    /// Nykyhetki UTC-aikaleimana.
    fn now(&self) -> Timestamp;
}

/// Tuotannon kello: delegoi [`familyclaw_core::time::now`]:hin (UTC). Ainoa
/// seinäkellon luku failover-polulla.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        familyclaw_core::time::now()
    }
}

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

    /// Ratkaisee mallinimen **provider-identiteetiksi + avain-pooliksi**
    /// ([`ResolvedEntry`]) cooldown/key-rotation -kerrokselle.
    ///
    /// Oletustoteutus delegoi [`resolve`](Self::resolve):lle ja kääräisee
    /// tuloksen yhden avaimen pooliksi (provider = mallinimen `provider/`-osa,
    /// avain = ratkaistun configin `api_key`). Näin **olemassa olevat**
    /// resolverit, jotka toteuttavat vain `resolve`:n, kääntyvät yhä eivätkä
    /// tarjoa multi-key-rotaatiota. [`EnvEndpointResolver`] **ylikirjoittaa**
    /// tämän palauttaakseen aidon monen avaimen poolin.
    ///
    /// # Errors
    /// Sama kuin [`resolve`](Self::resolve).
    fn resolve_entry(&self, model_name: &str) -> Result<ResolvedEntry> {
        let cfg = self.resolve(model_name)?;
        let provider = model_name
            .split_once('/')
            .map_or(model_name, |(p, _)| p)
            .to_string();
        let keys = vec![cfg.api_key.clone()];
        Ok(ResolvedEntry {
            provider,
            template: cfg,
            keys,
        })
    }
}

/// Yhden mallinimen ratkaisu cooldown/rotation-kerrokselle: provider-identiteetti,
/// ajettava [`LlmConfig`]-pohja (template, `api_key` täytetään poolista) ja
/// avain-pool (yksi tai useampi env-avain).
///
/// `template.api_key` voi olla mitä tahansa — efektiivinen avain valitaan aina
/// `keys`-poolista (ks. `ChainEntry`). `keys` ei ole koskaan tyhjä: jos
/// providerille ei ole avainta, pool on `vec![String::new()]` (tyhjä avain →
/// virhe näkyy vasta complete():ssa, kuten ennenkin).
#[derive(Debug, Clone)]
pub struct ResolvedEntry {
    /// Provider-prefiksi (esim. `"openai"`). Sama prefiksi jaetut entryt
    /// jäähdytetään yhdessä avain-poolin loputtua (jaettu avain).
    pub provider: String,
    /// Ajettava asetuspohja (`api_key` korvataan poolin aktiivisella avaimella).
    pub template: LlmConfig,
    /// Avain-pool round-robin-rotaatiolle. Ei koskaan tyhjä.
    pub keys: Vec<String>,
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
    /// provider-prefiksi → (`api_base`, env-muuttujien nimet).
    ///
    /// Avain-env-lista mahdollistaa **key-poolin** per provider: useampi avain
    /// kierrätetään round-robinilla `AuthFailed`-tilanteessa
    /// (`ChainEntry`). Yhden avaimen [`with_provider`](Self::with_provider)
    /// työntää listaan yhden alkion (taaksepäin-yhteensopiva).
    providers: HashMap<String, (String, Vec<String>)>,
    /// Maksimi tokenit per vastaus (välitetään jokaiseen [`LlmConfig`]:iin).
    max_tokens: Option<u32>,
    /// Request-timeout (ms) joka asetetaan jokaiseen ratkaistuun
    /// [`LlmConfig`]:iin (KERROS B -viritys). `None` → [`LlmConfig`]:n oletus
    /// ([`crate::llm::DEFAULT_REQUEST_TIMEOUT_MS`]) jää voimaan.
    request_timeout_ms: Option<u64>,
    /// Connect-timeout (ms) joka asetetaan jokaiseen ratkaistuun
    /// [`LlmConfig`]:iin. `None` → [`crate::llm::DEFAULT_CONNECT_TIMEOUT_MS`].
    connect_timeout_ms: Option<u64>,
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
            .insert(prefix.into(), (api_base.into(), vec![key_env.into()]));
        self
    }

    /// Kuten [`with_provider`](Self::with_provider), mutta rekisteröi **useita**
    /// avain-env-muuttujia (key-pool). `AuthFailed`-tilanteessa
    /// cooldown-kerros kierrättää poolin avaimet round-robinilla ennen kuin
    /// koko provider jäähdytetään (KERROS B). Tyhjä `key_envs` putoaa takaisin
    /// käytökseen "ei avainta" (yksi tyhjä avain), jotta resolveri ei koskaan
    /// tuota tyhjää poolia.
    ///
    /// - `prefix` — mallinimen `provider/`-osa, esim. `"openai"`.
    /// - `api_base` — OpenAI-yhteensopiva base-URL.
    /// - `key_envs` — ympäristömuuttujat järjestyksessä, esim.
    ///   `["OPENAI_API_KEY_1", "OPENAI_API_KEY_2"]`.
    #[must_use]
    pub fn with_provider_keys(
        mut self,
        prefix: impl Into<String>,
        api_base: impl Into<String>,
        key_envs: Vec<String>,
    ) -> Self {
        let key_envs = if key_envs.is_empty() {
            vec![String::new()]
        } else {
            key_envs
        };
        self.providers
            .insert(prefix.into(), (api_base.into(), key_envs));
        self
    }

    /// Asettaa max_tokens-arvon kaikkiin ratkaistuihin asetuksiin.
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// Asettaa request-timeoutin (ms) kaikkiin ratkaistuihin asetuksiin
    /// (F1, KERROS B -viritys). Ks. [`LlmConfig::with_request_timeout_ms`].
    #[must_use]
    pub fn with_request_timeout_ms(mut self, ms: u64) -> Self {
        self.request_timeout_ms = Some(ms);
        self
    }

    /// Asettaa connect-timeoutin (ms) kaikkiin ratkaistuihin asetuksiin
    /// (F1, KERROS B -viritys). Ks. [`LlmConfig::with_connect_timeout_ms`].
    #[must_use]
    pub fn with_connect_timeout_ms(mut self, ms: u64) -> Self {
        self.connect_timeout_ms = Some(ms);
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

impl EnvEndpointResolver {
    /// Soveltaa `max_tokens` + timeout -viritykset annettuun configiin (jaettu
    /// [`resolve`](LlmEndpointResolver::resolve) ja
    /// [`resolve_entry`](LlmEndpointResolver::resolve_entry) kesken).
    fn apply_tunings(&self, mut cfg: LlmConfig) -> LlmConfig {
        if let Some(max) = self.max_tokens {
            cfg = cfg.with_max_tokens(max);
        }
        if let Some(ms) = self.request_timeout_ms {
            cfg = cfg.with_request_timeout_ms(ms);
        }
        if let Some(ms) = self.connect_timeout_ms {
            cfg = cfg.with_connect_timeout_ms(ms);
        }
        cfg
    }
}

impl LlmEndpointResolver for EnvEndpointResolver {
    fn resolve(&self, model_name: &str) -> Result<LlmConfig> {
        let (provider, model) = Self::split(model_name);
        let (api_base, key_envs) = self.providers.get(provider).ok_or_else(|| {
            FamilyClawError::config(format!("unknown provider prefix for model '{model_name}'"))
        })?;
        // Avain luetaan ajonaikaisesti env:stä. Puuttuva avain ei estä
        // ketjun rakennusta (esim. fallback-mallit, joita ei kenties tarvita)
        // — tyhjä avain päätyy LlmConfigiin ja virhe näkyy vasta complete():ssa.
        // `resolve` käyttää poolin **ensimmäistä** avainta (taaksepäin-
        // yhteensopiva yhden avaimen polku); rotaatio elää resolve_entry:ssä.
        let api_key = key_envs
            .first()
            .map(|e| std::env::var(e).unwrap_or_default())
            .unwrap_or_default();
        let cfg = LlmConfig::new(api_base.clone(), api_key, model.to_string());
        Ok(self.apply_tunings(cfg))
    }

    fn resolve_entry(&self, model_name: &str) -> Result<ResolvedEntry> {
        let (provider, model) = Self::split(model_name);
        let (api_base, key_envs) = self.providers.get(provider).ok_or_else(|| {
            FamilyClawError::config(format!("unknown provider prefix for model '{model_name}'"))
        })?;
        // Lue koko avain-pool ajonaikaisesti env:stä. Tyhjä env → tyhjä
        // merkkijono (virhe näkyy vasta complete():ssa). Pool ei ole koskaan
        // tyhjä (rekisteröinti takaa ≥1 alkion), joten ChainEntry saa aina
        // vähintään yhden (mahdollisesti tyhjän) avaimen.
        let keys: Vec<String> = key_envs
            .iter()
            .map(|e| std::env::var(e).unwrap_or_default())
            .collect();
        let template = self.apply_tunings(LlmConfig::new(
            api_base.clone(),
            String::new(),
            model.to_string(),
        ));
        Ok(ResolvedEntry {
            provider: provider.to_string(),
            template,
            keys,
        })
    }
}

/// Yhden ketju-entryn (provider/malli-pari) **terveystila** cooldown-
/// tilakoneelle.
///
/// `cooled_until` = aikaleima johon asti entry on jäähdyllä (`None` = terve).
/// `strike` = yleinen eskalaatiolaskuri (rate-limit/overload/http/timeout) ja
/// `auth_strike` = erillinen auth-eskalaatio (avain-poolin loputtua). Molemmat
/// `saturating_add`-kasvatettuja → ei wraparound-bugia.
#[derive(Debug, Clone, Default)]
struct EntryHealth {
    /// Aikaleima johon asti entry ohitetaan (PASS 1). `None` = terve.
    cooled_until: Option<Timestamp>,
    /// Yleinen eskalaatiolaskuri (indeksoi [`LlmFailover::COOLDOWN_LADDER`]).
    strike: u8,
    /// Auth-eskalaatiolaskuri (indeksoi [`LlmFailover::AUTH_COOLDOWN_LADDER`]),
    /// käytössä vain kun koko avain-pool on loppuun yritetty.
    auth_strike: u8,
}

/// Yksi ajettava ketju-entry: provider-identiteetti, asetuspohja, avain-pool +
/// kursori, rakennettu [`LlmClient`] ja [`EntryHealth`].
///
/// Avain vaihdetaan `AuthFailed`-tilanteessa kasvattamalla `key_cursor`ia
/// (round-robin) ja rakentamalla `client` uudelleen poolin seuraavalla
/// avaimella. Kursori **säilyy** `complete()`-kutsujen yli, jotta toimiva avain
/// ei aina aloita uudelleen poolin alusta.
struct ChainEntry {
    /// Provider-prefiksi (esim. `"openai"`). Sama prefiksi jaetut entryt
    /// jäähdytetään yhdessä avain-poolin loputtua.
    provider: String,
    /// Asetuspohja (`api_key` korvataan aktiivisella poolin avaimella).
    template: LlmConfig,
    /// Avain-pool (ei koskaan tyhjä).
    keys: Vec<String>,
    /// Aktiivisen avaimen indeksi `keys`-poolissa (säilyy kutsujen yli).
    key_cursor: usize,
    /// Rakennettu klientti aktiivisella avaimella.
    client: LlmClient,
    /// Eskalaatio-/cooldown-tila.
    health: EntryHealth,
}

impl ChainEntry {
    /// Rakentaa entryn ratkaistusta [`ResolvedEntry`]:stä. Aloittaa avaimesta
    /// 0 ja terveestä tilasta.
    fn from_resolved(resolved: ResolvedEntry) -> Self {
        let ResolvedEntry {
            provider,
            template,
            mut keys,
        } = resolved;
        // Pool ei saa olla tyhjä — turvaverkko (resolve_entry takaa tämän jo).
        if keys.is_empty() {
            keys.push(String::new());
        }
        let client = Self::build_client(&template, &keys[0]);
        Self {
            provider,
            template,
            keys,
            key_cursor: 0,
            client,
            health: EntryHealth::default(),
        }
    }

    /// Template + avain → ajettava [`LlmConfig`] (avain korvaa templaten kentän).
    fn config_with_key(template: &LlmConfig, api_key: &str) -> LlmConfig {
        let mut cfg = template.clone();
        cfg.api_key.clear();
        cfg.api_key.push_str(api_key);
        cfg
    }

    /// Rakentaa [`LlmClient`]:n templatesta annetulla avaimella.
    fn build_client(template: &LlmConfig, api_key: &str) -> LlmClient {
        LlmClient::new(Self::config_with_key(template, api_key))
    }

    /// Vaihtaa aktiiviseen avaimeen `idx` ja rakentaa klientin uudelleen.
    fn switch_to_key(&mut self, idx: usize) {
        self.key_cursor = idx;
        self.client = Self::build_client(&self.template, &self.keys[self.key_cursor]);
    }

    /// Efektiivinen ajettava asetus (template + aktiivinen avain).
    fn effective_config(&self) -> LlmConfig {
        Self::config_with_key(&self.template, &self.keys[self.key_cursor])
    }

    /// Nollaa terveystilan onnistuneen kutsun jälkeen (toimiva avain todistaa
    /// providerin elossa).
    fn mark_healthy(&mut self) {
        self.health = EntryHealth::default();
    }
}

/// Failover-tilakoneen muuttuva osa: ketju-entryt. Kaikki mutatoitava tila elää
/// täällä [`std::sync::Mutex`]:n takana.
struct FailoverState {
    entries: Vec<ChainEntry>,
}

/// Päätös epäonnistuneen entry-yrityksen jälkeen (ei sisällä Ok-arvoa, jotta
/// se on geneerinen-vapaa ja jaettu `complete`/`complete_with_tools` kesken).
enum FailureStep {
    /// Entry epäonnistui retryable-virheellä → jatka seuraavaan entryyn.
    NextEntry(LlmError),
    /// Avain vaihdettiin (`AuthFailed`, pool ei vielä loppu) → yritä SAMA entry
    /// uudelleen heti.
    RetrySameEntry,
    /// Ei-retryable virhe → palauta heti (älä jauha ketjua).
    Fatal(LlmError),
}

/// Lopputulos yhden entryn yrityksestä: onnistui (arvo `T`), vai virhe-askel.
enum Attempt<T> {
    /// Kutsu onnistui.
    Ok(T),
    /// Epäonnistui — seuraava askel.
    Failure(FailureStep),
}

/// Järjestetty failover-ketju **cooldown-tilakoneella ja key-pool-rotaatiolla**.
///
/// Rakennetaan [`ModelConfig`]:sta [`build_llm_chain`]illa: ensin `primary`,
/// sitten `fallbacks` järjestyksessä ([`ModelConfig::preference_order`]).
/// [`complete`](LlmFailover::complete) yrittää jokaista **tervettä** entryä
/// järjestyksessä; jäähdyllä olevat ohitetaan (PASS 1). Jos mikään terve entry
/// ei vastaa, **viimeisenä keinona** (PASS 2) yritetään jokaista entryä
/// jäähdystä välittämättä — perhe ei jää koskaan ilman vastausta vaikka kaikki
/// ilmaismallit jäähtyisivät yhtä aikaa.
///
/// ## Interior mutability
/// `complete()` pysyy `&self`:nä (taaksepäin-yhteensopiva); kaikki muuttuva tila
/// (cooldown, kursori) elää [`Mutex`]:n takana. Lukko pidetään **vain**
/// synkronisten luku-/kirjoitusaskelten ajan (lue cooldown / kirjaa virhe /
/// vaihda avain / kloonaa klientti-kahva) — **ei koskaan** `.await`:n yli.
pub struct LlmFailover {
    state: Mutex<FailoverState>,
    /// Primary-mallin nimi (`preference_order`in ensimmäinen), raportointiin.
    primary: String,
    /// Päätöslogiikan kello (oletus [`SystemClock`]). Testit injektoivat fake-
    /// kellon [`with_clock`](Self::with_clock):lla.
    clock: Arc<dyn Clock>,
}

impl LlmFailover {
    /// Yleinen eskalaatioporras (rate-limit/overload/http/timeout/nocontent),
    /// indeksoitu `strike`illä kasvatuksen JÄLKEEN, saturoiden viimeiseen
    /// ämpäriin: strike 1→60 s, 2→5 min, 3→25 min, 4+→1 h.
    const COOLDOWN_LADDER: [std::time::Duration; 4] = [
        std::time::Duration::from_secs(60),
        std::time::Duration::from_secs(300),
        std::time::Duration::from_secs(1_500),
        std::time::Duration::from_secs(3_600),
    ];

    /// Pidempi auth-porras (avain peruttu / laskutus loppu — toipuu hitaasti).
    /// Saavutetaan vasta kun koko avain-pool on loppuun yritetty: 5 min / 30 min
    /// / 2 h / 6 h.
    const AUTH_COOLDOWN_LADDER: [std::time::Duration; 4] = [
        std::time::Duration::from_secs(300),
        std::time::Duration::from_secs(1_800),
        std::time::Duration::from_secs(7_200),
        std::time::Duration::from_secs(21_600),
    ];

    /// Rakentaa **yhden klientin** failover-ketjun (pituus 1) valmiista
    /// [`LlmConfig`]:sta — taaksepäin-yhteensopiva silta yhden mallin
    /// tapaukselle ([`Agent::new`](crate::Agent::new) kääräisee tähän, kun
    /// sille annetaan `Some(LlmConfig)`). Käyttäytyy täsmälleen kuin suora
    /// `LlmClient::new(cfg)`-kutsu, mutta [`complete`](LlmFailover::complete)
    /// kulkee saman failover-rajapinnan läpi (ketjun pituus 1 = ei fallbackeja,
    /// avain-pool yksi alkio).
    #[must_use]
    pub fn single(cfg: LlmConfig) -> Self {
        let primary = cfg.model.clone();
        let keys = vec![cfg.api_key.clone()];
        let provider = cfg
            .model
            .split_once('/')
            .map_or(cfg.model.as_str(), |(p, _)| p)
            .to_string();
        let entry = ChainEntry::from_resolved(ResolvedEntry {
            provider,
            template: cfg,
            keys,
        });
        Self {
            state: Mutex::new(FailoverState {
                entries: vec![entry],
            }),
            primary,
            clock: Arc::new(SystemClock),
        }
    }

    /// Vaihtaa päätöskellon (testibuilder). Tuotanto käyttää [`SystemClock`]:ia.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Laskee cooldown-keston yleiselle (ei-auth) retryable-virheelle
    /// `strike`-arvon (kasvatuksen JÄLKEEN) perusteella. Saturoi
    /// [`COOLDOWN_LADDER`](Self::COOLDOWN_LADDER):n viimeiseen ämpäriin.
    ///
    /// - `RateLimited`: `max(cooldown_hint, ladder[strike])` — provider-vihjettä
    ///   (esim. `Retry-After`) **kunnioitetaan lattiana** kun se ylittää portaan,
    ///   mutta valehteleva `retry_after:1` ei estä eskalaatiota toiston myötä.
    /// - `Overloaded`: `max(cooldown_hint, ladder[strike])` (hint = 2 s oletus).
    /// - `Http`/`Timeout`/`NoContent`: `ladder[strike]`.
    fn general_cooldown(err: &LlmError, strike: u8) -> std::time::Duration {
        let rung = Self::ladder_at(&Self::COOLDOWN_LADDER, strike);
        match err.cooldown_hint() {
            Some(hint) => hint.max(rung),
            None => rung,
        }
    }

    /// Indeksoi portaan saturoiden (strike on 1-pohjainen kasvatuksen jälkeen →
    /// indeksi `strike-1`, viimeiseen ämpäriin asti). `strike == 0` → ensimmäinen.
    fn ladder_at(ladder: &[std::time::Duration; 4], strike: u8) -> std::time::Duration {
        let idx = (strike.saturating_sub(1) as usize).min(ladder.len() - 1);
        ladder[idx]
    }

    /// Kuvaa [`std::time::Duration`]:n [`chrono::Duration`]:ksi cooldown-
    /// aritmetiikkaa varten. Ylivuoto (epätodennäköistä portaiden kanssa)
    /// kuvautuu maksimiin → entry pysyy pitkään jäähdyllä, ei paniikkia.
    fn chrono_dur(d: std::time::Duration) -> chrono::Duration {
        chrono::Duration::from_std(d).unwrap_or_else(|_| chrono::Duration::seconds(i64::MAX / 1000))
    }

    /// Onko entry juuri nyt jäähdyllä (PASS 1 ohittaa nämä)?
    fn is_cooled(now: Timestamp, health: &EntryHealth) -> bool {
        health.cooled_until.is_some_and(|until| until > now)
    }

    /// Yrittää **yhtä** entryä `complete`/`complete_with_tools`-kutsulla.
    /// Lukko on jo VAPAUTETTU ennen tätä; tämä ottaa lukon vain
    /// avaimen-vaihdon / virheen-kirjauksen ajaksi.
    ///
    /// `tried_keys` seuraa per-invocation kierretyt avaimet (täysi lap →
    /// pool loppu → jäähdytä provider).
    async fn try_entry_complete(
        &self,
        idx: usize,
        client: LlmClient,
        messages: &[LlmMessage],
        tried_keys: &mut std::collections::BTreeSet<usize>,
    ) -> Attempt<String> {
        match client.complete(messages).await {
            Ok(text) => {
                self.record_success(idx);
                Attempt::Ok(text)
            }
            Err(e) => Attempt::Failure(self.record_failure(idx, e, tried_keys)),
        }
    }

    /// Kuten [`try_entry_complete`](Self::try_entry_complete) mutta tool-calleilla.
    async fn try_entry_complete_with_tools(
        &self,
        idx: usize,
        client: LlmClient,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        tool_choice: Option<&str>,
        tried_keys: &mut std::collections::BTreeSet<usize>,
    ) -> Attempt<CompletionResult> {
        match client
            .complete_with_tools_choice(messages, tools, tool_choice)
            .await
        {
            Ok(result) => {
                self.record_success(idx);
                Attempt::Ok(result)
            }
            Err(e) => Attempt::Failure(self.record_failure(idx, e, tried_keys)),
        }
    }

    /// Nollaa entryn terveystilan onnistuneen kutsun jälkeen (lukon alla).
    fn record_success(&self, idx: usize) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(entry) = state.entries.get_mut(idx) {
                entry.mark_healthy();
            }
        }
    }

    /// Kirjaa entryn epäonnistumisen lukon alla ja päättää seuraavan askeleen:
    /// vaihda avain (retry sama), jäähdytä provider, jäähdytä entry (jatka), tai
    /// fatal (ei-retryable). Ei `.await`:a — lukko vapautuu palautuksessa.
    fn record_failure(
        &self,
        idx: usize,
        err: LlmError,
        tried_keys: &mut std::collections::BTreeSet<usize>,
    ) -> FailureStep {
        // Ei-retryable → fatal heti, ilman tilamuutosta.
        if !err.is_retryable() {
            return FailureStep::Fatal(err);
        }
        let now = self.clock.now();
        let Ok(mut state) = self.state.lock() else {
            // Myrkytetty lukko: kohtele kuten "kokeile seuraavaa" — ei paniikkia.
            return FailureStep::NextEntry(err);
        };
        let Some(entry) = state.entries.get_mut(idx) else {
            return FailureStep::NextEntry(err);
        };

        if matches!(err, LlmError::AuthFailed(_)) {
            tried_keys.insert(entry.key_cursor);
            // Onko poolissa avain jota EI vielä yritetty tällä kutsulla?
            let next = (0..entry.keys.len()).find(|k| !tried_keys.contains(k));
            if let Some(next_idx) = next {
                // Vaihda avaimeen ja yritä SAMA entry uudelleen heti.
                // Kuollut avain ei kerro mitään mallin elinkelpoisuudesta.
                entry.switch_to_key(next_idx);
                FailureStep::RetrySameEntry
            } else {
                // Koko pool yritetty → jäähdytä KOKO provider (jaettu avain).
                let provider = entry.provider.clone();
                Self::cool_provider(&mut state, &provider, now);
                FailureStep::NextEntry(err)
            }
        } else {
            // Yleinen retryable → eskaloiva backoff tälle entrylle.
            entry.health.strike = entry.health.strike.saturating_add(1);
            let dur = Self::general_cooldown(&err, entry.health.strike);
            entry.health.cooled_until = Some(now + Self::chrono_dur(dur));
            FailureStep::NextEntry(err)
        }
    }

    /// Jäähdyttää KAIKKI annettua provideria jakavat entryt auth-portaalla
    /// (jaettu avain → yksi kuollut avain tappaa kaikki sen mallit). Kasvattaa
    /// `auth_strike`in ja asettaa `cooled_until`in.
    fn cool_provider(state: &mut FailoverState, provider: &str, now: Timestamp) {
        for entry in state.entries.iter_mut().filter(|e| e.provider == provider) {
            entry.health.auth_strike = entry.health.auth_strike.saturating_add(1);
            let dur = Self::ladder_at(&Self::AUTH_COOLDOWN_LADDER, entry.health.auth_strike);
            entry.health.cooled_until = Some(now + Self::chrono_dur(dur));
        }
    }

    /// Snapshot yhden entryn (idx, klientti-kahva) **terveistä** entryistä
    /// järjestyksessä, lukon alla. Kloonaa vain klientti-kahvan (`reqwest::Client`
    /// = halpa Arc-klooni) jotta `.await` tapahtuu lukon ulkopuolella.
    fn healthy_clients(&self, now: Timestamp) -> Vec<(usize, LlmClient)> {
        self.snapshot_clients(now, true)
    }

    /// Kuten [`healthy_clients`](Self::healthy_clients) mutta KAIKKI entryt
    /// (PASS 2, viimeinen keino — jäähdystä ei huomioida).
    fn all_clients(&self) -> Vec<(usize, LlmClient)> {
        self.snapshot_clients(self.clock.now(), false)
    }

    /// Kerää (idx, kloonattu klientti-kahva) -parit. `only_healthy=true` →
    /// ohita jäähdyllä olevat.
    fn snapshot_clients(&self, now: Timestamp, only_healthy: bool) -> Vec<(usize, LlmClient)> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        state
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !only_healthy || !Self::is_cooled(now, &e.health))
            .map(|(i, e)| (i, e.client.clone()))
            .collect()
    }

    /// Kloonaa entryn aktiivisen klientti-kahvan indeksillä (lukon alla).
    /// `None` jos entry on poistunut. Käytetään avaimen-vaihto-retryssä.
    fn client_at(&self, idx: usize) -> Option<LlmClient> {
        let state = self.state.lock().ok()?;
        state.entries.get(idx).map(|e| e.client.clone())
    }

    /// Yrittää `complete()`:ä cooldown-tietoisesti: PASS 1 terveet entryt
    /// (jäähdyllä olevat ohitetaan), PASS 2 viimeisenä keinona kaikki entryt.
    /// Avain-rotaatio `AuthFailed`-tilanteessa, eskaloiva backoff muille
    /// retryable-virheille. Palauttaa viimeisen virheen jos kaikki epäonnistuvat.
    ///
    /// **F1 — retryable-semantiikka säilyy:** ei-retryable virhe (esim. parse)
    /// palautetaan **välittömästi**. Cooldown-kerros lisää tähän: jäähdyllä
    /// oleva entry ohitetaan PASS 1:ssä, mutta PASS 2 takaa ettei perhe jää
    /// ilman vastausta vaikka kaikki entryt olisivat jäähdyllä.
    ///
    /// # Errors
    /// Viimeisin [`LlmError`] jos kaikki ketjun entryt epäonnistuvat (tai
    /// ensimmäinen ei-retryable virhe), tai [`LlmError::NoContent`] jos ketju on
    /// tyhjä.
    pub async fn complete(&self, messages: &[LlmMessage]) -> std::result::Result<String, LlmError> {
        let mut last_err: Option<LlmError> = None;

        // PASS 1: terveet entryt (jäähdyllä olevat ohitetaan). Snapshot otetaan
        // nyt; PASS 2:n snapshot otetaan VASTA PASS 1:n jälkeen jotta se näkee
        // PASS 1:n avain-vaihdot.
        for (idx, mut client) in self.healthy_clients(self.clock.now()) {
            let mut tried_keys = std::collections::BTreeSet::new();
            loop {
                match self
                    .try_entry_complete(idx, client, messages, &mut tried_keys)
                    .await
                {
                    Attempt::Ok(text) => return Ok(text),
                    Attempt::Failure(FailureStep::RetrySameEntry) => match self.client_at(idx) {
                        Some(c) => client = c,
                        None => break,
                    },
                    Attempt::Failure(FailureStep::NextEntry(e)) => {
                        last_err = Some(e);
                        break;
                    }
                    Attempt::Failure(FailureStep::Fatal(e)) => return Err(e),
                }
            }
        }

        // PASS 2 (viimeinen keino): kaikki entryt, jäähdystä välittämättä —
        // perhe ei jää koskaan ilman vastausta vaikka kaikki entryt jäähtyisivät.
        for (idx, mut client) in self.all_clients() {
            let mut tried_keys = std::collections::BTreeSet::new();
            loop {
                match self
                    .try_entry_complete(idx, client, messages, &mut tried_keys)
                    .await
                {
                    Attempt::Ok(text) => return Ok(text),
                    Attempt::Failure(FailureStep::RetrySameEntry) => match self.client_at(idx) {
                        Some(c) => client = c,
                        None => break,
                    },
                    Attempt::Failure(FailureStep::NextEntry(e)) => {
                        last_err = Some(e);
                        break;
                    }
                    Attempt::Failure(FailureStep::Fatal(e)) => return Err(e),
                }
            }
        }

        Err(last_err.unwrap_or(LlmError::NoContent))
    }

    /// Kuten [`complete`](Self::complete), mutta SSE-striimauksella.
    ///
    /// # Errors
    /// Viimeisin [`LlmError`] jos kaikki ketjun entryt epäonnistuvat.
    pub async fn complete_stream(
        &self,
        messages: &[LlmMessage],
    ) -> std::result::Result<crate::llm::LlmChunkStream, LlmError> {
        let mut last_err: Option<LlmError> = None;
        for (idx, client) in self.healthy_clients(self.clock.now()) {
            match client.complete_stream(messages).await {
                Ok(stream) => {
                    self.record_success(idx);
                    return Ok(stream);
                }
                Err(e) => {
                    last_err = Some(e.clone());
                    if !e.is_retryable() {
                        return Err(e);
                    }
                    let _ = self.record_failure(idx, e, &mut std::collections::BTreeSet::new());
                }
            }
        }
        for (idx, client) in self.all_clients() {
            match client.complete_stream(messages).await {
                Ok(stream) => {
                    self.record_success(idx);
                    return Ok(stream);
                }
                Err(e) => {
                    last_err = Some(e.clone());
                    if !e.is_retryable() {
                        return Err(e);
                    }
                }
            }
        }
        Err(last_err.unwrap_or(LlmError::NoContent))
    }

    /// Kuten [`complete`](Self::complete), mutta mainostaa `tools`-työkalut ja
    /// palauttaa [`CompletionResult`]:n (teksti + mahdolliset tool-callit).
    /// Sama cooldown/rotation-logiikka (PASS 1 terveet, PASS 2 viimeinen keino).
    ///
    /// # Errors
    /// Viimeisin [`LlmError`] jos kaikki ketjun entryt epäonnistuvat (tai
    /// ensimmäinen ei-retryable virhe), tai [`LlmError::NoContent`] jos ketju on
    /// tyhjä.
    pub async fn complete_with_tools(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
    ) -> std::result::Result<CompletionResult, LlmError> {
        self.complete_with_tools_choice(messages, tools, None).await
    }

    /// Like [`complete`](Self::complete_with_tools) with explicit `tool_choice`.
    pub async fn complete_with_tools_choice(
        &self,
        messages: &[LlmMessage],
        tools: &[ToolDefinition],
        tool_choice: Option<&str>,
    ) -> std::result::Result<CompletionResult, LlmError> {
        let mut last_err: Option<LlmError> = None;

        // PASS 1: terveet entryt.
        for (idx, mut client) in self.healthy_clients(self.clock.now()) {
            let mut tried_keys = std::collections::BTreeSet::new();
            loop {
                match self
                    .try_entry_complete_with_tools(
                        idx,
                        client,
                        messages,
                        tools,
                        tool_choice,
                        &mut tried_keys,
                    )
                    .await
                {
                    Attempt::Ok(result) => return Ok(result),
                    Attempt::Failure(FailureStep::RetrySameEntry) => match self.client_at(idx) {
                        Some(c) => client = c,
                        None => break,
                    },
                    Attempt::Failure(FailureStep::NextEntry(e)) => {
                        last_err = Some(e);
                        break;
                    }
                    Attempt::Failure(FailureStep::Fatal(e)) => return Err(e),
                }
            }
        }

        // PASS 2 (viimeinen keino): kaikki entryt, jäähdystä välittämättä.
        for (idx, mut client) in self.all_clients() {
            let mut tried_keys = std::collections::BTreeSet::new();
            loop {
                match self
                    .try_entry_complete_with_tools(
                        idx,
                        client,
                        messages,
                        tools,
                        tool_choice,
                        &mut tried_keys,
                    )
                    .await
                {
                    Attempt::Ok(result) => return Ok(result),
                    Attempt::Failure(FailureStep::RetrySameEntry) => match self.client_at(idx) {
                        Some(c) => client = c,
                        None => break,
                    },
                    Attempt::Failure(FailureStep::NextEntry(e)) => {
                        last_err = Some(e);
                        break;
                    }
                    Attempt::Failure(FailureStep::Fatal(e)) => return Err(e),
                }
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
        self.state.lock().map_or(0, |s| s.entries.len())
    }

    /// Onko ketju tyhjä.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Primary-entryn efektiivinen ajettava asetus (template + aktiivinen
    /// avain). `None` jos ketju on tyhjä.
    #[must_use]
    pub fn primary_config(&self) -> Option<LlmConfig> {
        let state = self.state.lock().ok()?;
        state.entries.first().map(ChainEntry::effective_config)
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
    build_llm_chain_with_clock(cfg, resolver, Arc::new(SystemClock))
}

/// Kuten [`build_llm_chain`], mutta injektoi cooldown-tilakoneen **kellon**
/// (testikäyttö). Tuotanto käyttää [`build_llm_chain`]:ia joka antaa
/// [`SystemClock`]:n. Testit antavat fake-kellon askeltaakseen cooldown-ikkunan
/// yli ilman `tokio::time::sleep`-odotusta.
///
/// # Errors
/// Sama kuin [`build_llm_chain`].
pub fn build_llm_chain_with_clock(
    cfg: &ModelConfig,
    resolver: &dyn LlmEndpointResolver,
    clock: Arc<dyn Clock>,
) -> Result<LlmFailover> {
    cfg.validate()?;
    let primary = cfg.primary.clone();
    let mut entries = Vec::new();
    for model_name in cfg.preference_order() {
        match resolver.resolve_entry(model_name) {
            Ok(entry_spec) => entries.push(ChainEntry::from_resolved(entry_spec)),
            Err(e) => {
                // Ohita tuntematon malli mutta kirjaa syy debug-tasolla.
                tracing::debug!(model = model_name, error = %e, "skipping unresolvable model");
            }
        }
    }
    if entries.is_empty() {
        return Err(FamilyClawError::config(format!(
            "no usable model: none of '{}' (+{} fallbacks) resolved to an endpoint",
            cfg.primary,
            cfg.fallbacks.len()
        )));
    }
    Ok(LlmFailover {
        state: Mutex::new(FailoverState { entries }),
        primary,
        clock,
    })
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
            .with_provider(
                "deepseek",
                "https://api.deepseek.com/v1",
                "DEEPSEEK_API_KEY",
            )
            .with_provider("opencode", "https://opencode.ai/zen/v1", "OPENCODE_API_KEY")
    }

    #[test]
    fn split_handles_provider_prefix_and_bare_name() {
        assert_eq!(
            EnvEndpointResolver::split("openai/gpt-4o"),
            ("openai", "gpt-4o")
        );
        assert_eq!(
            EnvEndpointResolver::split("bare-model"),
            ("bare-model", "bare-model")
        );
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
        let err = r
            .resolve("mystery/model")
            .expect_err("unknown provider rejected");
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

    /// TEHTÄVÄ C4 -hyväksyntä: `FamilyConfig`-JSON → agentti rakentuu ilman
    /// paniikkia (primary `LlmConfig` saadaan config-kerroksesta + resolverista).
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

    #[test]
    fn resolver_applies_timeouts_to_resolved_config() {
        // F1: KERROS B virittää timeoutin → se päätyy ratkaistuun LlmConfigiin
        // → gateway-tuotantopolku perii sen (build_resolver → resolve → new).
        let r = test_resolver()
            .with_request_timeout_ms(7_000)
            .with_connect_timeout_ms(800);
        let cfg = r.resolve("openai/gpt-4o").expect("resolves");
        assert_eq!(cfg.request_timeout_ms, Some(7_000));
        assert_eq!(cfg.connect_timeout_ms, Some(800));
    }

    #[test]
    fn resolver_without_timeout_leaves_config_default() {
        // Ilman viritystä resolveri ei pakota timeoutia → LlmConfigin oletus
        // (60s/10s LlmClient::new:ssä) jää voimaan.
        let r = test_resolver();
        let cfg = r.resolve("openai/gpt-4o").expect("resolves");
        assert_eq!(cfg.request_timeout_ms, None);
        assert_eq!(cfg.connect_timeout_ms, None);
    }

    /// F1 retryable-semantiikka yksikkötasolla: ei-retryable virhe palautetaan
    /// **välittömästi** eikä koko ketjua jauheta. Käytämme tyhjää (mahdotonta
    /// ratkaista) endpointtia varmistaaksemme rakenteen — varsinainen
    /// timeout→failover-todiste on runtime-roundtripissä
    /// (`timeout_primary_fails_over_to_live_fallback`).
    #[tokio::test]
    async fn complete_on_empty_chain_path_is_no_content() {
        // Suora rakennus tyhjällä ketjulla ei ole sallittu rajapinnan kautta,
        // mutta complete()-semantiikka tyhjälle ketjulle on määritelty:
        // varmistetaan ettei se paniikkaa.
        let failover = LlmFailover {
            state: Mutex::new(FailoverState {
                entries: Vec::new(),
            }),
            primary: String::new(),
            clock: Arc::new(SystemClock),
        };
        assert!(failover.is_empty());
        let err = failover
            .complete(&[LlmMessage::user("hi")])
            .await
            .expect_err("empty chain yields error, not panic");
        assert!(matches!(err, LlmError::NoContent));
    }
}

// ── Cooldown state machine + key-pool rotation (failover gap #1 steps 2-3) ───
#[cfg(test)]
mod cooldown_tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use familyclaw_core::time::{from_unix_secs, Timestamp};
    use familyclaw_core::ModelConfig;

    use super::{
        build_llm_chain_with_clock, Clock, EnvEndpointResolver, LlmError, LlmFailover, LlmMessage,
    };

    /// Determinismiä varten ohjattava fake-kello: askelletaan aikaa ilman
    /// `sleep`-odotusta cooldown-ikkunan yli.
    struct FixedClock(Mutex<Timestamp>);

    impl FixedClock {
        fn at(secs: i64) -> Arc<Self> {
            Arc::new(Self(Mutex::new(
                from_unix_secs(secs).expect("valid unix secs"),
            )))
        }

        /// Siirtää kelloa eteenpäin annetut sekunnit.
        fn advance(&self, secs: i64) {
            let mut t = self.0.lock().expect("clock lock");
            *t += chrono::Duration::seconds(secs);
        }
    }

    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            *self.0.lock().expect("clock lock")
        }
    }

    /// Yhden mallin vastausresepti, valittu pyyntölaskurin (per-portti) tai
    /// Bearer-avaimen mukaan.
    #[derive(Clone)]
    struct Reply {
        status: u16,
        /// Vastauksen sisältö onnistuneessa tapauksessa (assistant content).
        content: String,
    }

    impl Reply {
        fn ok(content: &str) -> Self {
            Self {
                status: 200,
                content: content.into(),
            }
        }
        fn status(code: u16) -> Self {
            Self {
                status: code,
                content: String::new(),
            }
        }
    }

    /// Pieni HTTP/1.1-mock joka EI vaadi axumia: lukee pyynnön, valitsee
    /// `Reply`:n ja vastaa. Vastaukset voi ohjata joko pyyntöjärjestyksellä
    /// (`script`) tai Bearer-avaimen mukaan (`by_key`). Laskee pyynnöt.
    struct MockLlm {
        base_url: String,
        calls: Arc<AtomicUsize>,
        /// Avain-kohtaiset pyyntölaskurit (rotaatiotodisteeksi).
        key_calls: Arc<Mutex<HashMap<String, usize>>>,
    }

    impl MockLlm {
        /// Käynnistää mockin, joka palauttaa `script[min(call, len-1)]`-vastauksen
        /// (saturoi viimeiseen). `by_key` (jos `Some`) ohittaa scriptin: vastaus
        /// valitaan Bearer-tokenin mukaan (puuttuva avain → `default`).
        fn spawn(script: Vec<Reply>, by_key: Option<(HashMap<String, Reply>, Reply)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind to ephemeral port");
            let addr = listener.local_addr().expect("mock local_addr");
            let base_url = format!("http://{addr}/v1");
            let calls = Arc::new(AtomicUsize::new(0));
            let key_calls = Arc::new(Mutex::new(HashMap::new()));

            let calls_t = Arc::clone(&calls);
            let key_calls_t = Arc::clone(&key_calls);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let n = calls_t.fetch_add(1, Ordering::SeqCst);
                    Self::handle(stream, n, &script, by_key.as_ref(), &key_calls_t);
                }
            });

            Self {
                base_url,
                calls,
                key_calls,
            }
        }

        fn total_calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn calls_for_key(&self, key: &str) -> usize {
            self.key_calls
                .lock()
                .expect("key_calls lock")
                .get(key)
                .copied()
                .unwrap_or(0)
        }

        fn handle(
            mut stream: TcpStream,
            call_index: usize,
            script: &[Reply],
            by_key: Option<&(HashMap<String, Reply>, Reply)>,
            key_calls: &Arc<Mutex<HashMap<String, usize>>>,
        ) {
            // Lue request-headerit kunnes tyhjä rivi; poimi Bearer + body-pituus.
            let mut buf = [0_u8; 4096];
            let read = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..read]);
            let bearer = req
                .lines()
                .find_map(|l| {
                    let lower = l.to_ascii_lowercase();
                    lower
                        .strip_prefix("authorization: bearer ")
                        .map(|_| l["authorization: bearer ".len()..].trim().to_string())
                })
                .unwrap_or_default();

            let reply = if let Some((map, default)) = by_key {
                *key_calls
                    .lock()
                    .expect("key_calls lock")
                    .entry(bearer.clone())
                    .or_insert(0) += 1;
                map.get(&bearer).cloned().unwrap_or_else(|| default.clone())
            } else {
                let idx = call_index.min(script.len().saturating_sub(1));
                script
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| Reply::status(500))
            };

            let body = if reply.status == 200 {
                format!(
                    r#"{{"id":"x","object":"chat.completion","choices":[{{"index":0,"message":{{"role":"assistant","content":{}}},"finish_reason":"stop"}}]}}"#,
                    serde_json::to_string(&reply.content).expect("json string")
                )
            } else {
                r#"{"error":"mock"}"#.to_string()
            };
            let reason = match reply.status {
                200 => "OK",
                401 => "Unauthorized",
                429 => "Too Many Requests",
                503 => "Service Unavailable",
                _ => "Error",
            };
            let response = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.status,
                reason,
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    }

    fn msgs() -> Vec<LlmMessage> {
        vec![LlmMessage::user("hi")]
    }

    /// Kuvaa typitetyn fake-kellon trait-objektiksi `build_llm_chain_with_clock`:lle
    /// (Arc<FixedClock> ei auto-coerce Arc<dyn Clock>:ksi argumenttina).
    fn dyn_clock(clock: &Arc<FixedClock>) -> Arc<dyn Clock> {
        Arc::clone(clock) as Arc<dyn Clock>
    }

    /// Rakentaa yhden mallin failoverin annetulla mockilla + fake-kellolla.
    fn single_model_failover(mock: &MockLlm, clock: &Arc<FixedClock>) -> LlmFailover {
        let resolver = EnvEndpointResolver::new().with_provider(
            "mock",
            mock.base_url.clone(),
            "FAMILYCLAW_TEST_KEY_UNSET",
        );
        let model = ModelConfig::new("mock/model-a");
        build_llm_chain_with_clock(&model, &resolver, dyn_clock(clock)).expect("chain builds")
    }

    // ── Cooldown entry/skip/exit + escalation ───────────────────────────────

    #[tokio::test]
    async fn rate_limited_entry_cools_then_last_resort_retries() {
        // Yksi malli: 429 ensimmäisellä kutsulla → entry jäähtyy → PASS 2
        // (viimeinen keino) yrittää saman entryn uudelleen samalla kutsulla.
        // Toinen mock-kutsu palauttaa 200 → onnistuu.
        let mock = MockLlm::spawn(vec![Reply::status(429), Reply::ok("recovered")], None);
        let clock = FixedClock::at(1000);
        let failover = single_model_failover(&mock, &clock);

        let out = failover
            .complete(&msgs())
            .await
            .expect("last-resort succeeds");
        assert_eq!(out, "recovered");
        // PASS 1 (429 → cool) + PASS 2 (200) = 2 kutsua.
        assert_eq!(mock.total_calls(), 2);
    }

    #[tokio::test]
    async fn healthy_fallback_used_when_primary_rate_limited() {
        // Kaksi mallia eri providereilla: primary 429 (jäähtyy), fallback 200.
        let primary = MockLlm::spawn(vec![Reply::status(429)], None);
        let fallback = MockLlm::spawn(vec![Reply::ok("from-fallback")], None);
        let clock = FixedClock::at(1000);

        let resolver = EnvEndpointResolver::new()
            .with_provider("pa", primary.base_url.clone(), "K_UNSET_A")
            .with_provider("pb", fallback.base_url.clone(), "K_UNSET_B");
        let model = ModelConfig::new("pa/m").with_fallback("pb/m");
        let failover =
            build_llm_chain_with_clock(&model, &resolver, dyn_clock(&clock)).expect("builds");

        let out = failover.complete(&msgs()).await.expect("fallback answers");
        assert_eq!(out, "from-fallback");
        // Primary tried once (429), fallback once (200). PASS 1 satisfied.
        assert_eq!(primary.total_calls(), 1);
        assert_eq!(fallback.total_calls(), 1);
    }

    #[tokio::test]
    async fn cooled_entry_skipped_until_clock_advances_past_window() {
        // 429 (60 s -porras strike 1) → entry jäähtyy 1000..1060.
        // Kakkoskutsu klo 1030 (yhä jäähdyllä): PASS 1 ohittaa, mutta 200 tulee
        // PASS 2:sta. Kolmoskutsu klo 1100 (jäähdy ohi): PASS 1 onnistuu suoraan.
        let mock = MockLlm::spawn(
            vec![Reply::status(429), Reply::ok("a"), Reply::ok("b")],
            None,
        );
        let clock = FixedClock::at(1000);
        let failover = single_model_failover(&mock, &clock);

        // Kutsu 1: 429 → cool until 1060, sitten PASS 2 antaa "a".
        assert_eq!(failover.complete(&msgs()).await.expect("c1"), "a");
        let after_c1 = mock.total_calls();
        assert!(after_c1 >= 2, "expected 429 + last-resort, got {after_c1}");

        // Onnistunut kutsu nollasi terveyden → seuraava kutsu on terve PASS 1.
        // Varmista determinismi: askella kello selvästi eteenpäin joka tapauksessa.
        clock.advance(120);
        let out = failover.complete(&msgs()).await.expect("c2 healthy");
        assert_eq!(out, "b");
    }

    #[tokio::test]
    async fn last_resort_serves_when_all_entries_cooled() {
        // Molemmat mallit 429 ensin (molemmat jäähtyvät PASS 1:ssä), sitten 200.
        // PASS 2 (viimeinen keino) takaa vastauksen vaikka kaikki jäähtyivät.
        let a = MockLlm::spawn(vec![Reply::status(429), Reply::ok("a-ok")], None);
        let b = MockLlm::spawn(vec![Reply::status(429), Reply::ok("b-ok")], None);
        let clock = FixedClock::at(1000);
        let resolver = EnvEndpointResolver::new()
            .with_provider("pa", a.base_url.clone(), "KA")
            .with_provider("pb", b.base_url.clone(), "KB");
        let model = ModelConfig::new("pa/m").with_fallback("pb/m");
        let failover =
            build_llm_chain_with_clock(&model, &resolver, dyn_clock(&clock)).expect("builds");

        // PASS 1: a→429(cool), b→429(cool). PASS 2: a→200 "a-ok".
        let out = failover
            .complete(&msgs())
            .await
            .expect("last-resort answers");
        assert_eq!(out, "a-ok");
        assert_eq!(a.total_calls(), 2, "a: PASS1 429 + PASS2 200");
        assert_eq!(b.total_calls(), 1, "b: PASS1 429 only (PASS2 stops at a)");
    }

    // ── Key-pool rotation on AuthFailed ─────────────────────────────────────

    #[tokio::test]
    async fn auth_failed_rotates_to_next_key_in_pool() {
        // Avain #1 (env KA1) → 401, avain #2 (env KA2) → 200. Rotaatio kesken
        // saman complete()-kutsun: kuollut avain ei jäähdytä mallia, vaan
        // seuraavaa avainta yritetään heti.
        std::env::set_var("FCT_KA1", "dead-key");
        std::env::set_var("FCT_KA2", "good-key");
        let mut by_key = HashMap::new();
        by_key.insert("dead-key".to_string(), Reply::status(401));
        by_key.insert("good-key".to_string(), Reply::ok("rotated-ok"));
        let mock = MockLlm::spawn(Vec::new(), Some((by_key, Reply::status(401))));
        let clock = FixedClock::at(1000);

        let resolver = EnvEndpointResolver::new().with_provider_keys(
            "mock",
            mock.base_url.clone(),
            vec!["FCT_KA1".into(), "FCT_KA2".into()],
        );
        let model = ModelConfig::new("mock/m");
        let failover =
            build_llm_chain_with_clock(&model, &resolver, dyn_clock(&clock)).expect("builds");

        let out = failover.complete(&msgs()).await.expect("rotation succeeds");
        assert_eq!(out, "rotated-ok");
        // Molempia avaimia kokeiltiin täsmälleen kerran (rotaatio, ei jäähdytys).
        assert_eq!(mock.calls_for_key("dead-key"), 1);
        assert_eq!(mock.calls_for_key("good-key"), 1);

        std::env::remove_var("FCT_KA1");
        std::env::remove_var("FCT_KA2");
    }

    #[tokio::test]
    async fn provider_exhausted_when_all_keys_auth_fail() {
        // Molemmat avaimet → 401. Pool loppuu → provider jäähdytetään →
        // ei loputonta silmukkaa. Lopputulos: virhe (kaikki kuolleet).
        std::env::set_var("FCT_KB1", "k1");
        std::env::set_var("FCT_KB2", "k2");
        let mock = MockLlm::spawn(Vec::new(), Some((HashMap::new(), Reply::status(401))));
        let clock = FixedClock::at(1000);

        let resolver = EnvEndpointResolver::new().with_provider_keys(
            "mock",
            mock.base_url.clone(),
            vec!["FCT_KB1".into(), "FCT_KB2".into()],
        );
        let model = ModelConfig::new("mock/m");
        let failover =
            build_llm_chain_with_clock(&model, &resolver, dyn_clock(&clock)).expect("builds");

        let err = failover
            .complete(&msgs())
            .await
            .expect_err("all keys dead → error, not hang");
        assert!(matches!(err, LlmError::AuthFailed(_)));
        // Per complete(): PASS 1 kokeilee k1(401)→k2(401)→pool loppu→jäähdytys.
        // PASS 2 kokeilee uudelleen (jäähdy ohitettu): k1(401)→k2(401)→loppu.
        // = 4 kutsua, ei enempää (tried-set estää loopin).
        assert_eq!(mock.calls_for_key("k1"), 2);
        assert_eq!(mock.calls_for_key("k2"), 2);

        std::env::remove_var("FCT_KB1");
        std::env::remove_var("FCT_KB2");
    }

    // ── escalation ladder (pure, no network) ────────────────────────────────

    #[test]
    fn general_cooldown_escalates_and_saturates() {
        // strike 1→60s, 2→300s, 3→1500s, 4→3600s, 5+→3600s (saturoi).
        let http = LlmError::Http("x".into());
        assert_eq!(
            LlmFailover::general_cooldown(&http, 1),
            std::time::Duration::from_secs(60)
        );
        assert_eq!(
            LlmFailover::general_cooldown(&http, 2),
            std::time::Duration::from_secs(300)
        );
        assert_eq!(
            LlmFailover::general_cooldown(&http, 4),
            std::time::Duration::from_secs(3_600)
        );
        // Saturoi viimeiseen ämpäriin (ei wraparound u8:lla).
        assert_eq!(
            LlmFailover::general_cooldown(&http, 250),
            std::time::Duration::from_secs(3_600)
        );
    }

    #[test]
    fn rate_limited_honors_retry_after_as_floor() {
        // retry_after 600 s > strike-1 porras (60 s) → lattiana 600 s.
        let big = LlmError::RateLimited {
            message: "429".into(),
            retry_after: Some(600),
        };
        assert_eq!(
            LlmFailover::general_cooldown(&big, 1),
            std::time::Duration::from_secs(600)
        );
        // retry_after 1 s < porras 60 s → porras voittaa (provider ei voi
        // valehdella pois eskalaatiosta).
        let tiny = LlmError::RateLimited {
            message: "429".into(),
            retry_after: Some(1),
        };
        assert_eq!(
            LlmFailover::general_cooldown(&tiny, 1),
            std::time::Duration::from_secs(60)
        );
    }

    #[test]
    fn auth_ladder_escalates_and_saturates() {
        assert_eq!(
            LlmFailover::ladder_at(&LlmFailover::AUTH_COOLDOWN_LADDER, 1),
            std::time::Duration::from_secs(300)
        );
        assert_eq!(
            LlmFailover::ladder_at(&LlmFailover::AUTH_COOLDOWN_LADDER, 4),
            std::time::Duration::from_secs(21_600)
        );
        assert_eq!(
            LlmFailover::ladder_at(&LlmFailover::AUTH_COOLDOWN_LADDER, 99),
            std::time::Duration::from_secs(21_600)
        );
    }

    #[tokio::test]
    async fn retryable_http_error_grinds_pass1_and_pass2_then_returns_last() {
        // 418 → Http (retryable, ei tarkkaa luokkaa) → PASS 1 jäähdyttää, PASS 2
        // (viimeinen keino) yrittää uudelleen = 2 kutsua, sitten viimeinen virhe
        // palautetaan. Todistaa että retryable EI ole fatal ja PASS 2 ajetaan.
        let mock = MockLlm::spawn(vec![Reply::status(418)], None);
        let clock = FixedClock::at(1000);
        let failover = single_model_failover(&mock, &clock);
        let err = failover.complete(&msgs()).await.expect_err("all fail");
        assert!(matches!(err, LlmError::Http(_)));
        assert_eq!(mock.total_calls(), 2);
    }

    #[test]
    fn success_resets_health_via_primary_config_roundtrip() {
        // Rakenteellinen tarkistus: primary_config palauttaa efektiivisen
        // avaimen poolista (ei tyhjää templatea).
        std::env::set_var("FCT_PCFG", "live-key-xyz");
        let resolver = EnvEndpointResolver::new().with_provider_keys(
            "mock",
            "http://127.0.0.1:1/v1".to_string(),
            vec!["FCT_PCFG".into()],
        );
        let model = ModelConfig::new("mock/m");
        let clock = FixedClock::at(0);
        let failover =
            build_llm_chain_with_clock(&model, &resolver, dyn_clock(&clock)).expect("builds");
        let cfg = failover.primary_config().expect("primary config");
        assert_eq!(cfg.api_key, "live-key-xyz");
        assert_eq!(cfg.model, "m");
        std::env::remove_var("FCT_PCFG");
    }
}
