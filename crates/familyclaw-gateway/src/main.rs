//! # familyclaw-gateway
//!
//! **Gateway-binääri** — FamilyClaw-alustan (KERROS A, OSS) pitkäikäinen
//! prosessi: se sitoo HTTP-portin, tarjoaa elinvoima- ja valmiustarkistukset
//! (`/healthz`, `/readyz`), käynnistää [`FamilyRuntime`]:n (bus + agentti +
//! kanava + reply-pumppu) yhdellä [`build_family`]-kutsulla ja pysyy pystyssä
//! kunnes käyttäjä pyytää siistin sammutuksen (`Ctrl-C`).
//!
//! Tämä on C5-saumassa luvattu `build_family`-kokoojan **ohut kuori**:
//! [`build_family`] (`FamilyRuntime`) korvaa aiemman suoran
//! [`ResonanceBus::start`]-kutsun **yhdellä** kutsulla. HTTP-/sammutuskuori
//! pysyi muuttumattomana — bus-kahva luovutetaan `GatewayState`:lle ja
//! `Ctrl-C` laukaisee [`FamilyRuntime::shutdown`]:n (entisen `bus.stop()`:n
//! sijaan).
//!
//! ## OSS-raja (KERROS A)
//! Ei kovakoodattuja perheenjäsenten nimiä, avaimia eikä polkuja. **Kaikki**
//! ajonaikainen kokoonpano luetaan ympäristöstä (KERROS B):
//! - `FAMILYCLAW_GATEWAY_ADDR` — kuunteluosoite (oletus `127.0.0.1:8787`),
//! - `FAMILYCLAW_AGENT_NAME` — agentin näyttönimi (oletus `agent_a`),
//! - `FAMILYCLAW_AGENT_MODEL` — `"provider/model"` (oletus `provider/model`),
//! - `FAMILYCLAW_PROFILE_DIR` — sielun profiilihakemiston juuri (valinnainen),
//! - `FAMILYCLAW_TELEGRAM_CHANNEL_ID` — Telegram-kanavainstanssin tunniste,
//! - `FAMILYCLAW_REPLY_TARGET` — staattinen reply-kohde (Telegram chat-id),
//! - `TELEGRAM_BOT_TOKEN` — Telegram-botin token (vaadittu kanavalle),
//! - `FAMILYCLAW_PROVIDERS` — provider-taulu resolverille, muoto
//!   `prefix=base_url=KEY_ENV` puolipistein eroteltuna (valinnainen; ilman
//!   tätä agentti ajaa ilman LLM:ää).
//!
//! ## Ajaminen
//! ```bash
//! TELEGRAM_BOT_TOKEN=... \
//! FAMILYCLAW_TELEGRAM_CHANNEL_ID=tg-main \
//! FAMILYCLAW_REPLY_TARGET=123456789 \
//! cargo run -p familyclaw-gateway
//! # toinen pääte:
//! curl -i http://127.0.0.1:8787/healthz   # 200 OK
//! curl -i http://127.0.0.1:8787/readyz    # 200 OK (bus käynnissä)
//! ```

use std::net::SocketAddr;
use std::sync::Arc;

use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use familyclaw_agent::{resolve_profile_dir, EnvEndpointResolver, Soul};
use familyclaw_bus::BusHandle;
use familyclaw_channels::{Channel, TelegramChannel};
use familyclaw_core::{AgentConfig, FamilyClawError, ModelConfig, Result};
use familyclaw_runtime::{build_family, FamilyRuntime};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

/// Ympäristömuuttuja, joka määrää gatewayn kuunteluosoitteen.
const ADDR_ENV: &str = "FAMILYCLAW_GATEWAY_ADDR";

/// Agentin näyttönimi (env). Geneerinen oletus — ei perheenjäsentä.
const AGENT_NAME_ENV: &str = "FAMILYCLAW_AGENT_NAME";
/// Agentin malli `"provider/model"` (env).
const AGENT_MODEL_ENV: &str = "FAMILYCLAW_AGENT_MODEL";
/// Telegram-kanavainstanssin tunniste (env).
const TELEGRAM_CHANNEL_ID_ENV: &str = "FAMILYCLAW_TELEGRAM_CHANNEL_ID";
/// Staattinen reply-kohde — Telegram chat-id, johon vastaukset ohjataan (env).
const REPLY_TARGET_ENV: &str = "FAMILYCLAW_REPLY_TARGET";
/// Telegram-botin token (env). Vaadittu kun kanava kytketään.
const TELEGRAM_TOKEN_ENV: &str = "TELEGRAM_BOT_TOKEN";
/// Provider-taulu resolverille (env). Muoto: `prefix=base_url=KEY_ENV` eroteltuna `;`.
const PROVIDERS_ENV: &str = "FAMILYCLAW_PROVIDERS";

/// Geneeriset oletukset (KERROS A — ei perhe-/avain-/polkutietoa).
const DEFAULT_AGENT_NAME: &str = "agent_a";
const DEFAULT_AGENT_MODEL: &str = "provider/model";
const DEFAULT_BUS_NAME: &str = "familyclaw-gateway-bus";

/// Oletuskuunteluosoite, kun [`ADDR_ENV`] ei ole asetettu. Sidotaan
/// silmukkaosoitteeseen oletuksena (turvallinen oletus — ei altista
/// gatewayta verkolle ilman tietoista valintaa).
const DEFAULT_ADDR: &str = "127.0.0.1:8787";

/// Gatewayn jaettu ajonaikainen tila, johon HTTP-handlerit viittaavat.
///
/// Pidetään tarkoituksella pienenä. `bus` on `Some` kun Resonance Bus on
/// käynnistetty — `/readyz` raportoi valmiuden tämän perusteella.
#[derive(Clone)]
struct GatewayState {
    /// Resonance Bus -kahva. `Some` = bus käynnissä → valmius OK.
    bus: Option<BusHandle>,
}

/// Elinvoimatarkistus: vastaa aina `200 OK` kun prosessi pystyy palvelemaan
/// HTTP-pyyntöjä. Ei tarkista riippuvuuksia (vrt. [`readyz`]).
async fn healthz() -> &'static str {
    "ok"
}

/// Valmiustarkistus: `200 OK` vain kun Resonance Bus on käynnissä, muuten
/// `503 Service Unavailable`. Kuormantasaaja/orkestroija voi käyttää tätä
/// päättääkseen, ohjataanko liikennettä tälle instanssille.
async fn readyz(
    axum::extract::State(state): axum::extract::State<Arc<GatewayState>>,
) -> (StatusCode, &'static str) {
    if state.bus.is_some() {
        (StatusCode::OK, "ready")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

/// Rakentaa gatewayn HTTP-reitityksen jaetulla tilalla.
fn build_router(state: Arc<GatewayState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}

/// Ratkaisee kuunteluosoitteen ympäristömuuttujasta tai oletuksesta.
///
/// # Errors
/// [`FamilyClawError::Config`] jos osoite on jäsentymätön `SocketAddr`.
fn resolve_addr() -> Result<SocketAddr> {
    let raw = std::env::var(ADDR_ENV).unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    raw.parse::<SocketAddr>()
        .map_err(|e| FamilyClawError::config(format!("invalid {ADDR_ENV} '{raw}': {e}")))
}

/// Rakentaa LLM-resolverin [`PROVIDERS_ENV`]-muuttujasta (KERROS B).
///
/// Muoto: `prefix=base_url=KEY_ENV` puolipistein eroteltuna, esim.
/// `openai=https://api.openai.com/v1=OPENAI_API_KEY;deepseek=https://api.deepseek.com/v1=DEEPSEEK_API_KEY`.
/// Tyhjä/asettamaton muuttuja → tyhjä resolveri (agentti ajaa ilman LLM:ää).
/// Virheelliset rivit ohitetaan varoituksella — yksi typo ei kaada gatewayta.
fn build_resolver() -> EnvEndpointResolver {
    let mut resolver = EnvEndpointResolver::new();
    let Ok(spec) = std::env::var(PROVIDERS_ENV) else {
        return resolver;
    };
    for entry in spec.split(';').filter(|s| !s.trim().is_empty()) {
        let parts: Vec<&str> = entry.splitn(3, '=').map(str::trim).collect();
        if let [prefix, base_url, key_env] = parts.as_slice() {
            if !prefix.is_empty() && !base_url.is_empty() && !key_env.is_empty() {
                resolver = resolver.with_provider(*prefix, *base_url, *key_env);
                continue;
            }
        }
        warn!(
            entry,
            "ohitetaan kelvoton {PROVIDERS_ENV}-rivi (odotettu prefix=base_url=KEY_ENV)"
        );
    }
    resolver
}

/// Lataa agentin sielun profiilihakemistosta jos [`FAMILYCLAW_PROFILE_DIR`]
/// on asetettu; muuten paljas runko (geneerinen ydin, ei perhe-sielua).
///
/// [`FAMILYCLAW_PROFILE_DIR`]: familyclaw_agent::PROFILE_DIR_ENV
fn load_agent_soul(agent_name: &str) -> Soul {
    match resolve_profile_dir(None, agent_name) {
        Some(dir) => match familyclaw_agent::load_soul(&dir) {
            Ok(soul) => {
                info!(dir = %dir.display(), "sielu ladattu profiilihakemistosta");
                soul
            }
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "sielun lataus epäonnistui — paljas runko");
                Soul::from_essence(format!("I am {agent_name}, a FamilyClaw being."))
            }
        },
        None => Soul::from_essence(format!("I am {agent_name}, a FamilyClaw being.")),
    }
}

/// Käynnistää [`FamilyRuntime`]:n ympäristöstä luetulla kokoonpanolla
/// (KERROS B). Lukee agentin nimen, mallin, sielun, Telegram-kanavan ja
/// reply-kohteen env-muuttujista — mitään ei kovakoodata (KERROS A).
///
/// # Errors
/// - [`FamilyClawError::InvalidInput`] jos vaadittu env-muuttuja
///   ([`TELEGRAM_TOKEN_ENV`], [`TELEGRAM_CHANNEL_ID_ENV`],
///   [`REPLY_TARGET_ENV`]) puuttuu tai kanavan rakennus epäonnistuu.
/// - [`FamilyClawError`] (käännettynä) jos [`build_family`] epäonnistuu.
async fn start_runtime() -> Result<FamilyRuntime> {
    let agent_name =
        std::env::var(AGENT_NAME_ENV).unwrap_or_else(|_| DEFAULT_AGENT_NAME.to_string());
    let model = std::env::var(AGENT_MODEL_ENV).unwrap_or_else(|_| DEFAULT_AGENT_MODEL.to_string());

    let token = std::env::var(TELEGRAM_TOKEN_ENV)
        .map_err(|_| FamilyClawError::invalid_input(format!("{TELEGRAM_TOKEN_ENV} must be set")))?;
    let channel_id = std::env::var(TELEGRAM_CHANNEL_ID_ENV).map_err(|_| {
        FamilyClawError::invalid_input(format!("{TELEGRAM_CHANNEL_ID_ENV} must be set"))
    })?;
    let reply_target = std::env::var(REPLY_TARGET_ENV)
        .map_err(|_| FamilyClawError::invalid_input(format!("{REPLY_TARGET_ENV} must be set")))?;

    let channel = TelegramChannel::new(token, channel_id).map_err(FamilyClawError::from)?;
    let agent_cfg = AgentConfig::new(&agent_name, ModelConfig::new(model));
    let soul = load_agent_soul(&agent_name);
    let resolver = build_resolver();

    info!(agent = %agent_name, "kootaan FamilyRuntime (build_family)");
    build_family(
        Some(DEFAULT_BUS_NAME.to_string()),
        agent_cfg,
        soul,
        Box::new(channel) as Box<dyn Channel>,
        reply_target,
        &resolver,
    )
    .await
}

#[tokio::main]
async fn main() -> Result<()> {
    // Tracing: oletustaso info, ohitettavissa RUST_LOG-muuttujalla.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let addr = resolve_addr()?;
    info!(%addr, "familyclaw-gateway käynnistyy");

    // C5-sauma: yksi build_family-kutsu kokoaa bus + agentti + kanava +
    // reply-pumppu (FamilyRuntime). Bus-kahva luovutetaan GatewayState:lle;
    // HTTP-/sammutuskuori pysyy ennallaan (vain bus.stop() → runtime.shutdown()).
    let runtime = start_runtime().await?;
    info!("FamilyRuntime käynnissä (bus + agentti + kanava)");

    let state = Arc::new(GatewayState {
        bus: Some(runtime.bus().clone()),
    });
    let app = build_router(state);

    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| FamilyClawError::bus(format!("gateway failed to bind {addr}: {e}")))?;
    let bound = listener
        .local_addr()
        .map_err(|e| FamilyClawError::bus(format!("gateway local_addr failed: {e}")))?;
    info!(%bound, "gateway kuuntelee — /healthz ja /readyz valmiina");

    // Palvele kunnes Ctrl-C pyytää siistiä sammutusta.
    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    // Sammutus: pysäytä runtime siististi (keskeyttää taskit + pysäyttää busin)
    // riippumatta palvelun lopputuloksesta.
    info!("gateway sammuu — pysäytetään FamilyRuntime");
    runtime.shutdown();

    serve_result.map_err(|e| FamilyClawError::bus(format!("gateway serve error: {e}")))?;
    info!("familyclaw-gateway pysähtyi siististi");
    Ok(())
}

/// Odottaa sammutussignaalia (`Ctrl-C`). Palaa kun signaali saapuu, mikä
/// laukaisee axumin siistin sammutuksen.
async fn shutdown_signal() {
    match tokio::signal::ctrl_c().await {
        Ok(()) => info!("Ctrl-C vastaanotettu — aloitetaan siisti sammutus"),
        Err(e) => error!("ctrl_c-kuuntelu epäonnistui: {e} — sammutetaan silti"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_addr_parses_to_expected_port() {
        // Varmista että oletusosoite jäsentyy SocketAddriksi oikealle portille.
        let parsed: SocketAddr = DEFAULT_ADDR.parse().expect("default addr parses");
        assert_eq!(parsed.port(), 8787);
        assert!(parsed.ip().is_loopback(), "oletus sitoutuu loopbackiin");
    }

    #[tokio::test]
    async fn healthz_returns_ok() {
        // Health on riippumaton busista: vastaa aina "ok".
        assert_eq!(healthz().await, "ok");
    }

    #[tokio::test]
    async fn readyz_is_unavailable_without_bus_and_ok_with_bus() {
        use axum::extract::State;
        use familyclaw_bus::ResonanceBus;

        // Ilman busia: ei valmis (503).
        let not_ready = Arc::new(GatewayState { bus: None });
        let (status, _) = readyz(State(not_ready)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

        // Busin kanssa: valmis (200).
        let bus = ResonanceBus::start(None).await.expect("bus");
        let ready = Arc::new(GatewayState {
            bus: Some(bus.clone()),
        });
        let (status, _) = readyz(State(ready)).await;
        assert_eq!(status, StatusCode::OK);
        bus.stop();
    }

    #[test]
    fn build_router_constructs_without_panic() {
        // Reititin rakentuu (tyyppitason savutesti) molemmilla tiloilla.
        let _ = build_router(Arc::new(GatewayState { bus: None }));
    }
}
