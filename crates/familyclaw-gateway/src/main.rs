//! # familyclaw-gateway
//!
//! **Gateway-binääri** — FamilyClaw-alustan (KERROS A, OSS) pitkäikäinen
//! prosessi: se sitoo HTTP-portin, tarjoaa elinvoima- ja valmiustarkistukset
//! (`/healthz`, `/readyz`), käynnistää Resonance Busin (perheen affektiivinen
//! hermosto) ja pysyy pystyssä kunnes käyttäjä pyytää siistin sammutuksen
//! (`Ctrl-C`).
//!
//! Tämä on C5-saumassa luvatun `build_family`-kokoojan **ohut kuori**: kun
//! `build_family` (`FamilyRuntime`) myöhemmin valmistuu, tämä binääri vaihtaa
//! suoran [`ResonanceBus::start`]-kutsun siihen yhteen kutsuun ilman että
//! HTTP-/sammutuskuori muuttuu. Tällä hetkellä C5-sauma ei ole vielä olemassa
//! (ks. tehtäväkontrahti), joten gateway käynnistää busin suoraan julkisella
//! API:lla — tasan se osa jonka `build_family` lopulta kapseloi.
//!
//! ## OSS-raja (KERROS A)
//! Ei kovakoodattuja perheenjäsenten nimiä, avaimia eikä polkuja. Kuuntelu-
//! osoite tulee ympäristömuuttujasta (`FAMILYCLAW_GATEWAY_ADDR`), oletus
//! `127.0.0.1:8787`.
//!
//! ## Ajaminen
//! ```bash
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
use familyclaw_bus::{BusHandle, ResonanceBus};
use familyclaw_core::{FamilyClawError, Result};
use tokio::net::TcpListener;
use tracing::{error, info};

/// Ympäristömuuttuja, joka määrää gatewayn kuunteluosoitteen.
const ADDR_ENV: &str = "FAMILYCLAW_GATEWAY_ADDR";

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
    raw.parse::<SocketAddr>().map_err(|e| {
        FamilyClawError::config(format!("invalid {ADDR_ENV} '{raw}': {e}"))
    })
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

    // C5-sauma (build_family) ohuesti: käynnistä Resonance Bus.
    // Kun build_family valmistuu, tämä korvataan yhdellä build_family-kutsulla,
    // joka palauttaa FamilyRuntimen (bus + agentit + reply_rx). HTTP-/sammutus-
    // kuori pysyy ennallaan.
    let bus = ResonanceBus::start(Some("familyclaw-gateway-bus".to_string())).await?;
    info!("Resonance Bus käynnissä");

    let state = Arc::new(GatewayState {
        bus: Some(bus.clone()),
    });
    let app = build_router(state);

    let listener = TcpListener::bind(addr).await.map_err(|e| {
        FamilyClawError::bus(format!("gateway failed to bind {addr}: {e}"))
    })?;
    let bound = listener
        .local_addr()
        .map_err(|e| FamilyClawError::bus(format!("gateway local_addr failed: {e}")))?;
    info!(%bound, "gateway kuuntelee — /healthz ja /readyz valmiina");

    // Palvele kunnes Ctrl-C pyytää siistiä sammutusta.
    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await;

    // Sammutus: pysäytä bus siististi riippumatta palvelun lopputuloksesta.
    info!("gateway sammuu — pysäytetään Resonance Bus");
    bus.stop();

    serve_result
        .map_err(|e| FamilyClawError::bus(format!("gateway serve error: {e}")))?;
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
