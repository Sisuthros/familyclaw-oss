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
//! `ResonanceBus::start`-kutsun **yhdellä** kutsulla. HTTP-/sammutuskuori
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
//! - `FAMILYCLAW_GATEWAY_TOKEN` — valinnainen bearer-token, joka suojaa
//!   `POST /inject`:n (asetettuna pyyntö vaatii `Authorization: Bearer <token>`;
//!   tyhjänä endpoint pysyy loopback-only-avoimena kuten ennen),
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
//!
//! ## Operaattorin hyväksyntäpinta (suspend/resume-silta, roadmap §6 D2)
//! Kun agentin tool-loop keskeytyy odottamaan ihmisen hyväksyntää
//! ([`ThinkOutcome::Suspended`](familyclaw_agent::ThinkOutcome::Suspended)),
//! käyttäjälle EI lähde vastausta — keskeytys on **operaattorin** asia. Gateway
//! tarjoaa kaksi bearer-suojattua reittiä (sama [`GATEWAY_TOKEN_ENV`]-token
//! kuin `/inject`):
//! - `GET /approvals/pending` — listaa odottavat hyväksynnät **redaktoituina**
//!   (`approval_id`, `redacted_summary`, `created_at`) — ei koskaan raakaa
//!   payloadia eikä salaisuuksia.
//! - `POST /approvals/{approval_id}/approve` — myöntää hyväksynnän ja ajaa
//!   keskeytyneen toiminnon loppuun (payload-sidottu, kertakäyttöinen).
//!
//! ```bash
//! TOKEN=...   # FAMILYCLAW_GATEWAY_TOKEN
//! curl -s -H "Authorization: Bearer $TOKEN" \
//!   http://127.0.0.1:8787/approvals/pending
//! # → [{"approval_id":"…","redacted_summary":"taito '…' odottaa …","created_at":"…"}]
//! curl -s -X POST -H "Authorization: Bearer $TOKEN" \
//!   http://127.0.0.1:8787/approvals/<approval_id>/approve
//! # → {"approval_id":"…","task_id":"…","status":"done","awaiting_further_approval":false}
//! ```

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Json, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::Router;
use clap::{Parser, Subcommand};
use familyclaw_actions::{ActionRuntime, ApprovalId, AuditCollector};
use familyclaw_agent::{resolve_profile_dir, EnvEndpointResolver, LiveTurnExecutor, Soul};
use familyclaw_bridge::{
    AgentInfo, AgentRole, FamilyBridge, HostKind, OrchestrationPlan, Orchestrator, TaskNode,
};
use familyclaw_bus::BusHandle;
use tokio::sync::Mutex;
mod config;
use config::FamilyConfig;
use familyclaw_channels::{
    verify_signature, Channel, ChannelKind, ChannelResult, DiscordChannel, DiscordInteraction,
    InboundMessage, MessageStream, OutboundMessage, SendFuture, TelegramChannel,
    RESPONSE_DEFERRED_CHANNEL_MESSAGE, RESPONSE_PONG,
};
use familyclaw_core::{AgentConfig, FamilyClawError, ModelConfig, Result};
use familyclaw_runtime::{build_family, FamilyRuntime};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

/// Ympäristömuuttuja, joka määrää gatewayn kuunteluosoitteen.
const ADDR_ENV: &str = "FAMILYCLAW_GATEWAY_ADDR";

/// Telegram-botin token (env). Vaadittu kun kanava kytketään.
/// (Muut env-väliaineet palveluvan kautta `FamilyConfig` — nähdään `config.rs`.)
const TELEGRAM_TOKEN_ENV: &str = "TELEGRAM_BOT_TOKEN";

/// Provider-taulu resolverille (env). Muoto: `prefix=base_url=KEY_ENV` eroteltuna `;`.
const PROVIDERS_ENV: &str = "FAMILYCLAW_PROVIDERS";

/// Env-nimet virheviesteissä (ei lueta suoraan — `FamilyConfig` hoitaa)
const DISCORD_WEBHOOK_URL_ENV: &str = "DISCORD_WEBHOOK_URL";
const DISCORD_PUBLIC_KEY_ENV: &str = "DISCORD_PUBLIC_KEY";
const DISCORD_CHANNEL_ID_ENV: &str = "DISCORD_CHANNEL_ID";
const TELEGRAM_CHANNEL_ID_ENV: &str = "FAMILYCLAW_TELEGRAM_CHANNEL_ID";
const REPLY_TARGET_ENV: &str = "FAMILYCLAW_REPLY_TARGET";

/// Valinnainen bearer-token, joka suojaa `POST /inject`:n (env). Käytetään
/// vain virheviesteissä/dokumentaatiossa — varsinainen arvo luetaan
/// `FamilyConfig`:n kautta. Vrt. `OpenClaw`in `OPENCLAW_GATEWAY_TOKEN`.
const GATEWAY_TOKEN_ENV: &str = "FAMILYCLAW_GATEWAY_TOKEN";

/// `orchestrate`-alikomennon suunnitelma JSON-muodossa. Tyhjä/asettamaton →
/// pieni sisäänrakennettu savutesti-suunnitelma. Muoto:
/// `{"id":"plan","nodes":[{"id":"n1","title":"...","description":"...","input":{...}}]}`.
const PLAN_ENV: &str = "FAMILYCLAW_PLAN";

/// Oletusarvot joita `FamilyConfig` käyttää (KERROS B).
const DEFAULT_BUS_NAME: &str = "familyclaw-gateway-bus";

/// Oletuskuunteluosoite, kun [`ADDR_ENV`] ei ole asetettu. Sidotaan
/// silmukkaosoitteeseen oletuksena (turvallinen oletus — ei altista
/// gatewayta verkolle ilman tietoista valintaa).
const DEFAULT_ADDR: &str = "127.0.0.1:8787";

/// FamilyClaw-gatewayn komentorivikäyttöliittymä.
///
/// Ilman alikomentoa gateway käyttäytyy kuten ennen CLI:tä — käynnistää
/// palvelimen (`serve`). Tämä säilyttää taaksepäinyhteensopivuuden
/// `cargo run -p familyclaw-gateway`- ja Docker-`CMD`-kutsuihin, jotka eivät
/// anna argumentteja.
#[derive(Parser)]
#[command(name = "familyclaw-gateway", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

/// Gatewayn alikomennot.
#[derive(Subcommand)]
enum Command {
    /// Käynnistä gateway-palvelin (oletus, kun alikomentoa ei anneta).
    Serve,
    /// Kysy käynnissä olevan gatewayn tila (`/healthz` + `/readyz`).
    ///
    /// Lukee [`ADDR_ENV`]:n (tai oletusosoitteen) ja tekee HTTP-pyynnöt.
    /// Tulostaa tilan ja palaa exit-koodilla `0` vain kun `/readyz` = 200.
    Status,
    /// Tarkista kokoonpano käynnistämättä palvelinta (offline-diagnostiikka).
    ///
    /// Vahvistaa kuunteluosoitteen jäsentymisen, portin vapauden ja vaaditut
    /// ympäristömuuttujat. Salaisuuksista raportoidaan **vain läsnäolo**
    /// (asetettu/puuttuu) — arvoja ei koskaan tulosteta.
    Doctor,
    /// Aja monivaiheinen orkesterointisuunnitelma kerran ja tulosta raportti.
    ///
    /// Tämä on multi-agent DAG -ajon **elävä sisäänkäynti**: kokoaa
    /// [`FamilyBridge`]:n, rekisteröi työntekijät, valitsee mallin
    /// ([`LiveTurnExecutor`] oikealla LLM-ketjulla [`build_resolver`]:n kautta)
    /// ja ajaa [`Orchestrator::run_with`]:n. Suunnitelma luetaan
    /// [`PLAN_ENV`]-ympäristömuuttujasta (JSON) tai käytetään pientä
    /// sisäänrakennettua oletussuunnitelmaa savutestiksi.
    ///
    /// **Rehellinen rajaus:** ajaa bridgen omalla substraatilla
    /// (`EventBus` + `AgentRegistry` + `TaskBoard`), EI [`FamilyRuntime`]:n
    /// ractor-agenteilla/`ResonanceBus`illa. Tämä tekee DAG-orkesteroinnista
    /// ajettavan oikeilla LLM-kutsuilla; fuusio eläviin runtime-agentteihin on
    /// erillinen, isompi työ.
    Orchestrate,
}

/// Gatewayn jaettu ajonaikainen tila, johon HTTP-handlerit viittaavat.
///
/// Pidetään tarkoituksella pienenä. `bus` on `Some` kun Resonance Bus on
/// käynnistetty — `/readyz` raportoi valmiuden tämän perusteella.
#[derive(Clone)]
struct GatewayState {
    /// Resonance Bus -kahva. `Some` = bus käynnissä → valmius OK.
    bus: Option<BusHandle>,
    /// Discord-kanava inject-handlerille. `Some` kun kanavatyyppi on "discord".
    discord_channel: Option<Arc<DiscordChannel>>,
    /// Valinnainen `POST /inject`-bearer-token. `Some` = endpoint vaatii
    /// `Authorization: Bearer <token>`:n; `None` = avoin loopback-only-oletus
    /// (yhteensopiva aiemman käytöksen kanssa). Vrt. `OpenClaw`in
    /// `OPENCLAW_GATEWAY_TOKEN`.
    inject_token: Option<Arc<str>>,
    /// Discord Interactions Ed25519 public key (hex). `Some` → `/discord/interactions` aktiivinen.
    discord_public_key: Option<Arc<str>>,
    /// **Jaettu toimintoajoympäristö** operaattorin hyväksyntäpinnalle
    /// (`GET /approvals/pending`, `POST /approvals/{id}/approve`).
    ///
    /// Sama [`Arc<Mutex<ActionRuntime>>`] jonka [`FamilyRuntime`] kytki agentin
    /// tool-looppiin ([`FamilyRuntime::actions`]) — operaattori ja agentti
    /// jakavat SAMAN lukitun tilan, joten gateway näkee tarkalleen ne odottavat
    /// hyväksynnät jotka agentin keskeytynyt vuoro jätti, ja `approve` ajaa
    /// keskeytyneen toiminnon loppuun samassa tilassa.
    ///
    /// `Some` palvelevassa gatewayssa (aina, [`build_family`] luo
    /// toimintoajoympäristön); `None` vain tiloissa joissa runtimea ei ole
    /// kytketty (esim. testit, jotka eivät tarvitse hyväksyntäpintaa). Kun
    /// `None`, hyväksyntäreitit vastaavat `503 Service Unavailable`.
    actions: Option<Arc<Mutex<ActionRuntime>>>,
    /// **Jaettu turn-audit-keräin** havainnoitavalle tool-loop-jäljelle
    /// (`GET /turns/audit`, TURN-AUDIT roadmap §6 D6).
    ///
    /// Sama [`Arc<AuditCollector>`] jonka [`build_family`] kytki agentin
    /// tool-looppiin ([`FamilyRuntime::turn_audit`]) — operaattori näkee
    /// tarkalleen ne tapahtumat jotka agentin vuorot kirjasivat (vuoron alku,
    /// työkalukutsut **redaktoituina**, suspend/resume, `stop_reason`).
    ///
    /// `Some` palvelevassa gatewayssa; `None` tiloissa joissa runtimea ei ole
    /// kytketty (esim. testit). Kun `None`, audit-reitti vastaa
    /// `503 Service Unavailable`.
    turn_audit: Option<Arc<AuditCollector>>,
}

/// Yhden odottavan hyväksynnän **operaattorille turvallinen, redaktoitu**
/// esitys `GET /approvals/pending`:n JSON-vastaukseen.
///
/// Tämä on tarkoituksella oma tyyppinsä eikä `familyclaw-actions`:n
/// sisäinen rakenne: se kantaa **vain** kolme salaisuudetonta kenttää jotka
/// operaattori tarvitsee päättääkseen hyväksynnästä — **ei koskaan raakaa
/// payloadia, työkaluargumentteja eikä salaisuuksia**. `redacted_summary` tulee
/// suoraan [`ActionRuntime::pending_summary_for`]:lta (johdettu vain taidon
/// nimestä + tunnisteista), ja `created_at` on auditointiaikaleima.
#[derive(serde::Serialize)]
struct PendingApprovalView {
    /// Hyväksynnän tunniste (`POST /approvals/{approval_id}/approve` jatkaa).
    approval_id: String,
    /// Redaktoitu ihmisluettava tiivistelmä (ei payloadia, ei salaisuuksia).
    redacted_summary: String,
    /// Hetki jolloin odottava kirjaus luotiin (RFC 3339 -aikaleima).
    created_at: String,
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

/// Vakioaikainen tavujonojen vertailu (defense-in-depth bearer-tokenille).
///
/// Palauttaa `true` vain jos jonot ovat samanpituiset ja tavuittain identtiset.
/// Suoritusaika riippuu vain pidemmän jonon pituudesta, ei sisällöstä — emme
/// oikosulje ensimmäisestä eroavasta tavusta, jottei vertailu vuoda
/// ajoituskanavaa hyökkääjälle (sama idiomi kuin `familyclaw-security`:n
/// ankkuritiivisteen vertailussa).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Tarkistaa `POST /inject`:n bearer-token-valtuutuksen.
///
/// - Jos tokenia **ei** ole konfiguroitu ([`GatewayState::inject_token`] =
///   `None`), pyyntö hyväksytään sellaisenaan (avoin loopback-only-oletus,
///   taaksepäinyhteensopiva).
/// - Jos token **on** konfiguroitu, otsikon `Authorization: Bearer <token>`
///   on oltava läsnä ja täsmättävä vakioaikaisesti — muuten
///   [`StatusCode::UNAUTHORIZED`].
///
/// Token-arvoja ei koskaan lokiteta (MEMORY.md secret-leak-sääntö).
///
/// Huom: paluutyyppi on `std::result::Result` täsmällisesti, koska tämän
/// kraatin laajuudessa `Result` viittaa [`familyclaw_core::Result`]-aliakseen.
fn check_inject_auth(
    state: &GatewayState,
    headers: &HeaderMap,
) -> std::result::Result<(), StatusCode> {
    let Some(expected) = state.inject_token.as_deref() else {
        // Ei tokenia konfiguroitu → avoin oletus (loopback-only).
        return Ok(());
    };
    // Pura `Authorization: Bearer <token>` — puuttuva/virheellinen otsikko = 401.
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim);
    match presented {
        Some(tok) if constant_time_eq(tok.as_bytes(), expected.as_bytes()) => Ok(()),
        _ => {
            tracing::warn!("inject: hylätty 401 — puuttuva tai väärä bearer-token");
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

/// Injektoi ulkopuolisen viestin Discord-kanavaan.
/// `POST /inject` — JSON: `{"sender": "...", "chat_id": "...", "body": "..."}`
///
/// Jos [`GATEWAY_TOKEN_ENV`] on konfiguroitu, pyyntö vaatii otsikon
/// `Authorization: Bearer <token>` (vakioaikainen täsmäys), muuten `401`.
async fn inject_discord(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> (StatusCode, &'static str) {
    if let Err(code) = check_inject_auth(&state, &headers) {
        return (code, "unauthorized");
    }
    let Some(ch) = &state.discord_channel else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "discord channel not configured",
        );
    };
    let sender = payload
        .get("sender")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let chat_id = payload
        .get("chat_id")
        .and_then(|v| v.as_str())
        .unwrap_or("dm");
    let body = payload.get("body").and_then(|v| v.as_str()).unwrap_or("");
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "body must not be empty");
    }
    let msg = match InboundMessage::new(sender, chat_id, body) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("invalid inbound message: {e}");
            return (StatusCode::BAD_REQUEST, "invalid message");
        }
    };
    let envelope = msg.into_envelope(ChannelKind::Discord, ch.channel_id());
    match ch.inject(envelope) {
        Ok(()) => (StatusCode::OK, "injected"),
        Err(e) => {
            tracing::warn!("discord inject failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "inject failed")
        }
    }
}

/// Discord Interactions endpoint — Ed25519-verify + inject + deferred vastaus.
async fn handle_discord_interaction(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(public_key) = state.discord_public_key.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "discord interactions not configured"})),
        );
    };
    let Some(ch) = state.discord_channel.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "discord channel not configured"})),
        );
    };

    let sig = headers
        .get("X-Signature-Ed25519")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let timestamp = headers
        .get("X-Signature-Timestamp")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if sig.is_empty() || timestamp.is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "missing signature headers"})),
        );
    }

    if let Err(e) = verify_signature(public_key, sig, timestamp, &body) {
        tracing::warn!("discord interaction verify failed: {e}");
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid signature"})),
        );
    }

    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("discord interaction json parse failed: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::json!({"type": 4, "data": {"content": "invalid payload", "flags": 64}}),
                ),
            );
        }
    };

    let interaction = match DiscordInteraction::from_payload(&payload) {
        Ok(ix) => ix,
        Err(e) => {
            tracing::warn!("discord interaction parse failed: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::json!({"type": 4, "data": {"content": "invalid interaction", "flags": 64}}),
                ),
            );
        }
    };

    if interaction.is_ping() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({"type": RESPONSE_PONG})),
        );
    }

    if !interaction.is_application_command() {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({"type": 4, "data": {"content": "unsupported interaction type", "flags": 64}}),
            ),
        );
    }

    let inbound = match interaction.into_inbound() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("discord slash empty message: {e}");
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::json!({"type": 4, "data": {"content": "message required", "flags": 64}}),
                ),
            );
        }
    };

    let envelope = inbound.into_envelope(ChannelKind::Discord, ch.channel_id());
    if let Err(e) = ch.inject(envelope) {
        tracing::warn!("discord interaction inject failed: {e}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"type": 4, "data": {"content": "inject failed", "flags": 64}})),
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({"type": RESPONSE_DEFERRED_CHANNEL_MESSAGE})),
    )
}

/// `GET /approvals/pending` — listaa operaattorille **redaktoituina** ne vuorot
/// jotka odottavat ihmisen hyväksyntää (suspend/resume-silta, roadmap §6 D2).
///
/// Vastaus on JSON-lista [`PendingApprovalView`]-objekteja, kukin sisältäen
/// **vain** kolme salaisuudetonta kenttää: `approval_id`, `redacted_summary` ja
/// `created_at`. **Raakaa payloadia, työkaluargumentteja tai salaisuuksia ei
/// koskaan palauteta** — lähde on [`ActionRuntime::try_pending_approvals`] +
/// [`ActionRuntime::pending_summary_for`]/[`ActionRuntime::pending_created_at_for`],
/// jotka kaikki johtavat tiedon vain redaktoidusta `PendingRecord`:stä
/// (actions-kerroksen salaisuudeton tallennusmuoto).
///
/// Suojaus on sama kuin `POST /inject`:llä: jos [`GATEWAY_TOKEN_ENV`] on
/// konfiguroitu, pyyntö vaatii otsikon `Authorization: Bearer <token>`
/// (vakioaikainen täsmäys), muuten `401`.
///
/// Tilakoodit:
/// - `200 OK` + JSON-lista (myös tyhjä lista, jos mikään ei odota),
/// - `401 Unauthorized` jos bearer-token vaaditaan eikä se täsmää,
/// - `503 Service Unavailable` jos toimintoajoympäristöä ei ole kytketty
///   ([`GatewayState::actions`] = `None`).
async fn list_pending_approvals(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    if check_inject_auth(&state, &headers).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        );
    }
    let Some(actions) = state.actions.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "action runtime not configured" })),
        );
    };

    // Lukko vain listauksen ajaksi. `try_pending_approvals` palauttaa vain
    // (approval_id, task_id); rikastamme sen redaktoidulla tiivistelmällä ja
    // luontihetkellä SAMAN lukon alla, jottei tila ehdi muuttua välissä.
    let rt = actions.lock().await;
    let pending = match rt.try_pending_approvals() {
        Ok(p) => p,
        Err(e) => {
            // Tallennuspinnan lukuvirhe (käytännössä vain kaatumiskestävällä
            // pinnalla). Ei vuoda yksityiskohtia operaattorin ulkopuolelle.
            warn!("approvals: pending-listan luku epäonnistui: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "failed to read pending approvals" })),
            );
        }
    };
    let views: Vec<PendingApprovalView> = pending
        .iter()
        .map(|p| {
            // Redaktoitu tiivistelmä + luontihetki samalta tallennuspinnalta.
            // `None` (kilpa: kulutettiin lukon ulkopuolella) → neutraali oletus,
            // ei koskaan raakaa dataa.
            let redacted_summary = rt
                .pending_summary_for(p.approval_id)
                .unwrap_or_else(|| "odottaa ihmisen hyväksyntää".to_string());
            let created_at = rt
                .pending_created_at_for(p.approval_id)
                .map_or_else(String::new, |t| t.to_rfc3339());
            PendingApprovalView {
                approval_id: p.approval_id.to_string(),
                redacted_summary,
                created_at,
            }
        })
        .collect();
    drop(rt);

    info!(count = views.len(), "approvals: listattiin odottavat hyväksynnät (redaktoituina)");
    let body = serde_json::to_value(&views).unwrap_or_else(|_| serde_json::json!([]));
    (StatusCode::OK, Json(body))
}

/// `POST /approvals/{approval_id}/approve` — myöntää hyväksynnän annetulle
/// `approval_id`:lle ja **ajaa keskeytyneen toiminnon loppuun** (suspend/resume-
/// silta, roadmap §6 D2).
///
/// Rungolla ei ole pakollista sisältöä (valinnainen). Hyväksyntä kulutetaan
/// tehtävän tallennettua payloadia vasten (payload-sidonta + kertakäyttö
/// [`ActionRuntime::approve`]:ssa), joten muutettu payload ei voi käyttää
/// hyväksyntää — eikä runko siksi voi vuotaa salaisuuksia suoritukseen.
///
/// Suojaus on sama kuin `POST /inject`:llä (bearer-token jos konfiguroitu).
///
/// Tilakoodit (**fail-closed, ei paniikkia**):
/// - `200 OK` + JSON-yhteenveto resumen lopputuloksesta (tehtävän tunniste +
///   tila, esim. `done`/`needs_approval` jos jatko vaati uuden hyväksynnän),
/// - `400 Bad Request` jos `approval_id` ei jäsenny kelvolliseksi tunnisteeksi,
/// - `401 Unauthorized` jos bearer-token vaaditaan eikä täsmää,
/// - `404 Not Found` jos tunnistetta ei (enää) odota hyväksyntä (tuntematon tai
///   jo kulutettu),
/// - `410 Gone` jos hyväksyntä on vanhentunut (TTL umpeutunut),
/// - `503 Service Unavailable` jos toimintoajoympäristöä ei ole kytketty.
async fn approve_pending(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Path(approval_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    if check_inject_auth(&state, &headers).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        );
    }
    let Some(actions) = state.actions.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "action runtime not configured" })),
        );
    };

    // Jäsennä tunniste (UUID). Kelvoton muoto = 400, ei 404 — eri syy.
    let Ok(id) = ApprovalId::from_str(approval_id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid approval id" })),
        );
    };

    // Determinismi (D1): aikaleima injektoidaan tähän yhteen pisteeseen ja ohjaa
    // sekä vanhentumistarkistuksen että keskeytyneen toiminnon suorituksen.
    let now = familyclaw_core::time::now();

    // Esitarkistus SAMAN lukon alla kuin approve: erotellaan "tuntematon" (404)
    // ja "vanhentunut" (410) ennen kuin yritämme kuluttaa hyväksynnän — muuten
    // molemmat näkyisivät `ActionRuntime::approve`:n ApprovalMissing-virheenä,
    // emmekä voisi erottaa 404:ää 410:stä fail-closed-tarkkuudella.
    let mut rt = actions.lock().await;
    match rt.pending_expiry_for(id) {
        None => {
            // Tunnistetta ei odota hyväksyntä (tuntematon tai jo kulutettu).
            warn!(approval = %id, "approvals: approve hylätty 404 — tuntematon tai kulutettu");
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "no such pending approval" })),
            );
        }
        Some(expires_at) if now > expires_at => {
            // Vanhentunut → 410 Gone (fail-closed, ei kuluteta sivuvaikutusta).
            warn!(approval = %id, "approvals: approve hylätty 410 — hyväksyntä vanhentunut");
            return (
                StatusCode::GONE,
                Json(serde_json::json!({ "error": "approval expired" })),
            );
        }
        Some(_) => {}
    }

    // Myönnä hyväksyntä ja aja keskeytynyt toiminto loppuun. Tämä on resumen
    // toiminto-puoli: payload-sidottu, kertakäyttöinen suoritus.
    match rt.approve(id, now).await {
        Ok(outcome) => {
            drop(rt);
            // Operaattorille palautetaan VAIN tehtävän tunniste + tila — ei
            // todistepakettia, ei payloadia. `status` kertoo onnistuiko resume
            // loppuun asti vai vaatiko jatko uuden hyväksynnän.
            let status = format!("{:?}", outcome.status).to_lowercase();
            info!(
                task = %outcome.task_id,
                status = %status,
                "approvals: hyväksyntä myönnetty, keskeytynyt toiminto ajettu"
            );
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "approval_id": approval_id,
                    "task_id": outcome.task_id.to_string(),
                    "status": status,
                    "awaiting_further_approval": outcome.awaiting_approval(),
                })),
            )
        }
        Err(e) => {
            drop(rt);
            // Esitarkistus ohitti tuntemattoman/vanhentuneen; jäljelle jää lähinnä
            // payload-mismatch tai putken virhe. Fail-closed 409, ei paniikkia,
            // ei salaisuuksia virheviestiin.
            warn!(approval = %id, error = %e, "approvals: approve epäonnistui putkessa");
            (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": "approval could not be granted" })),
            )
        }
    }
}

/// `GET /turns/audit` — palauttaa **havainnoitavan tool-loop-jäljen**
/// operaattorille (TURN-AUDIT, roadmap §6 D6).
///
/// Vastaus on JSON-lista [`familyclaw_actions::ExecAuditEvent`]-tapahtumia
/// lisäysjärjestyksessä: kukin kantaa vuoron korrelaatiotunnisteen
/// (`action_id`), tapahtumatyypin (`kind`: `turn_started` / `tool_dispatched`
/// / `turn_suspended` / `turn_resumed` / `turn_answered` /
/// `turn_max_iterations`), aikaleiman (`at`) ja **redaktoidun** selitteen
/// (`detail`). **Raakaa payloadia, työkaluargumentteja tai salaisuuksia ei
/// koskaan palauteta** — `detail` redaktoitiin jo agentin kirjaushetkellä.
///
/// Operaattori voi ryhmitellä jäljen `action_id`:n mukaan saadakseen yhden
/// vuoron koko elinkaaren (alku → työkalukutsut → suspend/resume →
/// `stop_reason`). Suuremmalla volyymilla suodatus/sivutus kuuluu myöhempään
/// laajennukseen — tämä reitti palauttaa nykyisen kirjatun jäljen sellaisenaan.
///
/// Suojaus on sama kuin `POST /inject`:llä: jos [`GATEWAY_TOKEN_ENV`] on
/// konfiguroitu, pyyntö vaatii otsikon `Authorization: Bearer <token>`
/// (vakioaikainen täsmäys), muuten `401`.
///
/// Tilakoodit:
/// - `200 OK` + JSON-lista (myös tyhjä, jos mitään ei ole vielä kirjattu),
/// - `401 Unauthorized` jos bearer-token vaaditaan eikä se täsmää,
/// - `503 Service Unavailable` jos turn-auditia ei ole kytketty
///   ([`GatewayState::turn_audit`] = `None`).
async fn list_turn_audit(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
) -> (StatusCode, Json<serde_json::Value>) {
    if check_inject_auth(&state, &headers).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        );
    }
    let Some(audit) = state.turn_audit.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "turn audit not configured" })),
        );
    };

    // Tapahtumat ovat jo redaktoituja (agentti redaktoi `detail`:n
    // kirjaushetkellä). Serialisoidaan sellaisenaan — ei lisäkäsittelyä.
    let events = audit.list();
    info!(count = events.len(), "turns: listattiin redaktoitu tool-loop-audit-jälki");
    let body = serde_json::to_value(&events).unwrap_or_else(|_| serde_json::json!([]));
    (StatusCode::OK, Json(body))
}

/// Rakentaa gatewayn HTTP-reitityksen jaetulla tilalla.
fn build_router(state: Arc<GatewayState>) -> Router {
    use axum::routing::post;
    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/inject", post(inject_discord))
        // Operaattorin hyväksyntäpinta (suspend/resume-silta, roadmap §6 D2).
        // Rekisteröidään aina; kun toimintoajoympäristöä ei ole kytketty
        // ([`GatewayState::actions`] = `None`), handlerit vastaavat 503.
        // Bearer-suojaus on sama kuin /inject:llä (`check_inject_auth`).
        .route("/approvals/pending", get(list_pending_approvals))
        .route("/approvals/{approval_id}/approve", post(approve_pending))
        // Havainnoitava tool-loop-jälki (TURN-AUDIT, roadmap §6 D6). Rekisteröidään
        // aina; kun turn-auditia ei ole kytketty ([`GatewayState::turn_audit`] =
        // `None`), handler vastaa 503. Bearer-suojaus on sama kuin /inject:llä.
        .route("/turns/audit", get(list_turn_audit));
    if state.discord_public_key.is_some() && state.discord_channel.is_some() {
        router = router.route("/discord/interactions", post(handle_discord_interaction));
    }
    router.with_state(state)
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

/// Jaettu-instanssi-adapteri: käärii `Arc<DiscordChannel>`:n `Channel`-trait-
/// olioksi delegoimalla kaikki kutsut SAMAAN instanssiin.
///
/// **Miksi tämä on olemassa (dual-instance-bugin korjaus):** bus-pumppu
/// ([`build_family`] → `channel.receive()`) ja inject-polut (`/inject`,
/// `/discord/interactions` → `Arc<DiscordChannel>::inject`) on aiemmin
/// rakennettu KAHDESTA erillisestä [`DiscordChannel::from_webhook`]-kutsusta.
/// Kukin kutsu luo oman `mpsc`-parin (`inbound_tx`/`inbound_rx`), joten
/// injektoidut viestit työnnettiin instanssiin #1:n `inbound_tx`:ään, jonka
/// `inbound_rx`:ää kukaan ei koskaan kuluttanut — webhook-injektointi katosi
/// mustaan aukkoon.
///
/// Tämä adapteri antaa rakentaa kanavan **kerran** (`Arc<DiscordChannel>`) ja
/// jakaa SAMAN instanssin: bus saa adapterin (`Box<dyn Channel>`), inject saa
/// `Arc`-kahvan. `receive()`/`send()`/`inject()` ottavat kaikki `&self`, joten
/// ne operoivat yhden instanssin samaa `inbound_tx`/`inbound_rx`-paria
/// vasten — juuri se yksi-virta-malli, jonka `DiscordChannel::inject`:n
/// dokumentaatio jo lupaa.
struct SharedDiscordChannel(Arc<DiscordChannel>);

impl Channel for SharedDiscordChannel {
    fn channel_id(&self) -> &str {
        self.0.channel_id()
    }

    fn kind(&self) -> ChannelKind {
        self.0.kind()
    }

    fn send(&self, message: OutboundMessage) -> SendFuture<'_> {
        self.0.send(message)
    }

    fn receive(&self) -> ChannelResult<MessageStream> {
        self.0.receive()
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
///
/// Palauttaa runtimen, Discord-kanavan (inject/interactions), inject-tokenin ja public keyn.
async fn start_runtime() -> Result<(
    FamilyRuntime,
    Option<Arc<DiscordChannel>>,
    Option<Arc<str>>,
    Option<Arc<str>>,
)> {
    let cfg = FamilyConfig::load()?;
    let agent_name = cfg.agent_name().to_string();
    let model = cfg.model().to_string();
    let channel_kind = cfg.channel_kind().to_string();

    // /inject-suojaus: tyhjä token = avoin loopback-only-oletus (varoitus),
    // asetettu token = pakollinen bearer-täsmäys. Arvoa ei koskaan lokiteta.
    let inject_token: Option<Arc<str>> = {
        let raw = cfg.gateway_token().trim();
        if raw.is_empty() {
            warn!(
                "{GATEWAY_TOKEN_ENV} ei asetettu — POST /inject on suojaamaton \
                 (luota loopback-sidontaan). Aseta token tuotannossa."
            );
            None
        } else {
            info!("POST /inject suojattu bearer-tokenilla ({GATEWAY_TOKEN_ENV})");
            Some(Arc::from(raw))
        }
    };

    let (channel, discord_ch): (Box<dyn Channel>, Option<Arc<DiscordChannel>>) =
        if channel_kind == "discord" {
            let webhook_url = cfg.discord_webhook_url();
            if webhook_url.is_empty() {
                return Err(FamilyClawError::invalid_input(format!(
                    "{DISCORD_WEBHOOK_URL_ENV} must be set for discord channel"
                )));
            }
            let ch_id = cfg.discord_channel_id();
            // Rakenna DiscordChannel TÄSMÄLLEEN KERRAN ja jaa sama instanssi.
            // Bus-pumppu saa `SharedDiscordChannel`-adapterin (Box<dyn Channel>),
            // inject-polut saavat `Arc`-kahvan — molemmat osoittavat samaan
            // `inbound_tx`/`inbound_rx`-pariin. Aiempi koodi rakensi KAKSI
            // erillistä instanssia, jolloin injektoidut viestit katosivat (dual-
            // instance-mustaaukko); ks. SharedDiscordChannel-dokumentaatio.
            let dc = DiscordChannel::from_webhook(webhook_url.to_string(), ch_id.to_string())
                .map_err(FamilyClawError::from)?;
            let dc_arc = Arc::new(dc);
            let ch: Box<dyn Channel> = Box::new(SharedDiscordChannel(Arc::clone(&dc_arc)));
            (ch, Some(dc_arc))
        } else {
            let token = cfg.telegram_token();
            if token.is_empty() {
                return Err(FamilyClawError::invalid_input(format!(
                    "{TELEGRAM_TOKEN_ENV} must be set"
                )));
            }
            let ch_id = cfg.telegram_channel_id();
            if ch_id.is_empty() {
                return Err(FamilyClawError::invalid_input(format!(
                    "{TELEGRAM_CHANNEL_ID_ENV} must be set"
                )));
            }
            let tc = TelegramChannel::new(token.to_string(), ch_id.to_string())
                .map_err(FamilyClawError::from)?;
            let ch: Box<dyn Channel> = Box::new(tc);
            (ch, None)
        };

    let reply_target = cfg.reply_target();
    if reply_target.is_empty() {
        return Err(FamilyClawError::invalid_input(format!(
            "{REPLY_TARGET_ENV} must be set"
        )));
    }
    let reply_target = reply_target.to_string();

    // VAKAA olennotunniste: johdetaan deterministisesti agentin nimestä, EI
    // arvota satunnaisesti. `AgentConfig::new` arpoo id:n joka prosessin
    // käynnistyksessä — silloin agentin `being_id` muuttuisi joka restartissa,
    // ja kaatumiskestävälle pinnalle ennen kaatumista tallennettu jatkettava
    // vuoro EI enää täsmäisi heränneen agentin omistajuustarkistukseen (oma
    // suspendoitu vuoro näyttäisi "toiselle olennolle kuuluvalta" eikä sitä
    // voisi koskaan jatkaa). Nimestä johdettu id pysyy vakaana yli restartin.
    let agent_cfg = AgentConfig::new_with_stable_id(&agent_name, ModelConfig::new(model));
    let soul = load_agent_soul(&agent_name);
    let resolver = build_resolver();

    info!(agent = %agent_name, channel = %channel_kind, "kootaan FamilyRuntime (build_family)");
    let runtime = build_family(
        Some(DEFAULT_BUS_NAME.to_string()),
        agent_cfg,
        soul,
        channel,
        reply_target,
        &resolver,
    )
    .await?;

    let discord_public_key: Option<Arc<str>> = if channel_kind == "discord" {
        let pk = cfg.discord_public_key().trim();
        if pk.is_empty() {
            warn!("{DISCORD_PUBLIC_KEY_ENV} puuttuu — POST /discord/interactions ei ole käytössä");
            None
        } else {
            info!("Discord Interactions aktiivinen ({DISCORD_PUBLIC_KEY_ENV} set)");
            Some(Arc::from(pk))
        }
    } else {
        None
    };

    Ok((runtime, discord_ch, inject_token, discord_public_key))
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

    // Ilman alikomentoa = serve (taaksepäinyhteensopivuus).
    match Cli::parse().command.unwrap_or(Command::Serve) {
        Command::Serve => serve().await,
        Command::Status => status().await,
        Command::Doctor => doctor().await,
        Command::Orchestrate => orchestrate().await,
    }
}

/// Käynnistää gateway-palvelimen ja pysyy pystyssä `Ctrl-C`:hen asti.
///
/// Tämä on entinen `main`-runko muuttumattomana: yksi [`build_family`]-kutsu
/// kokoaa busin + agentin + kanavan + reply-pumpun (`FamilyRuntime`), HTTP-kuori
/// sitoo portin ja siisti sammutus pysäyttää runtimen.
///
/// # Errors
/// [`FamilyClawError`] jos kokoonpano, sidonta tai palvelu epäonnistuu.
async fn serve() -> Result<()> {
    let addr = resolve_addr()?;
    info!(%addr, "familyclaw-gateway käynnistyy");

    // C5-sauma: yksi build_family-kutsu kokoaa bus + agentti + kanava +
    // reply-pumppu (FamilyRuntime). Bus-kahva luovutetaan GatewayState:lle;
    // HTTP-/sammutuskuori pysyy ennallaan (vain bus.stop() → runtime.shutdown()).
    let (runtime, discord_ch, inject_token, discord_public_key) = start_runtime().await?;
    info!("FamilyRuntime käynnissä (bus + agentti + kanava)");

    // Operaattorin hyväksyntäpinta jakaa SAMAN Arc<Mutex<ActionRuntime>>-kahvan
    // jonka build_family kytki agentin tool-looppiin — odottavat hyväksynnät
    // (suspend) ja niiden myöntäminen (resume) tapahtuvat samassa lukitussa
    // tilassa. Vrt. roadmap §6 D2.
    let actions = Some(runtime.actions());
    // Havainnoitava tool-loop-jälki (TURN-AUDIT, roadmap §6 D6): sama
    // Arc<AuditCollector> jonka build_family kytki agentin tool-looppiin.
    let turn_audit = Some(runtime.turn_audit());

    let state = Arc::new(GatewayState {
        bus: Some(runtime.bus().clone()),
        discord_channel: discord_ch,
        inject_token,
        discord_public_key,
        actions,
        turn_audit,
    });
    info!("operaattorin hyväksyntäpinta valmis — GET /approvals/pending, POST /approvals/{{id}}/approve");
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
    runtime.shutdown().await;

    serve_result.map_err(|e| FamilyClawError::bus(format!("gateway serve error: {e}")))?;
    info!("familyclaw-gateway pysähtyi siististi");
    Ok(())
}

/// Muodostaa `http://<addr><path>`-URL:n kuunteluosoitteesta.
///
/// Käynnissä oleva gateway sitoutuu oletuksena loopbackiin, joten `status`
/// olettaa `http`-skeeman (ei TLS:ää) — sama oletus kuin palvelimen sidonnassa.
fn health_url(addr: SocketAddr, path: &str) -> String {
    format!("http://{addr}{path}")
}

/// Kysyy käynnissä olevan gatewayn tilan (`/healthz` + `/readyz`).
///
/// Lukee kuunteluosoitteen [`resolve_addr`]:n kautta ja tekee kaksi HTTP
/// GET -pyyntöä. Tulostaa kummankin endpointin tilan. Palaa `Ok(())` vain
/// kun `/readyz` vastaa `200 OK`; muuten [`FamilyClawError::bus`], jolloin
/// prosessi päättyy nollasta poikkeavalla exit-koodilla.
///
/// # Errors
/// - [`FamilyClawError::config`] jos kuunteluosoite on jäsentymätön.
/// - [`FamilyClawError::bus`] jos gatewayyn ei saada yhteyttä tai `/readyz`
///   ei ole `200`.
async fn status() -> Result<()> {
    let addr = resolve_addr()?;
    let client = reqwest::Client::new();

    let health = client
        .get(health_url(addr, "/healthz"))
        .send()
        .await
        .map_err(|e| FamilyClawError::bus(format!("gateway not reachable at {addr}: {e}")))?;
    let health_ok = health.status().is_success();
    println!("healthz {addr} -> {}", health.status());

    let ready = client
        .get(health_url(addr, "/readyz"))
        .send()
        .await
        .map_err(|e| FamilyClawError::bus(format!("gateway not reachable at {addr}: {e}")))?;
    let ready_status = ready.status();
    println!("readyz  {addr} -> {ready_status}");

    if health_ok && ready_status.as_u16() == 200 {
        println!("status: ready");
        Ok(())
    } else {
        Err(FamilyClawError::bus(format!(
            "gateway not ready (healthz ok={health_ok}, readyz={ready_status})"
        )))
    }
}

/// Tarkistaa gatewayn kokoonpannon offline (käynnistämättä palvelinta).
///
/// Suorittaa kolme tarkistusta ja tulostaa kunkin tuloksen:
/// 1. **addr** — [`resolve_addr`] jäsentää kuunteluosoitteen,
/// 2. **port** — osoite saadaan väliaikaisesti sidottua (portti vapaa),
/// 3. **env** — vaaditut ympäristömuuttujat ovat asetettu.
///
/// Salaisuuksista (esim. [`TELEGRAM_TOKEN_ENV`]) raportoidaan **vain läsnäolo**
/// (`set`/`MISSING`) — arvoja ei tulosteta (MEMORY.md secret-leak-sääntö).
///
/// # Errors
/// [`FamilyClawError::invalid_input`] jos jokin tarkistus epäonnistuu, jolloin
/// prosessi päättyy nollasta poikkeavalla exit-koodilla.
async fn doctor() -> Result<()> {
    let cfg = FamilyConfig::load()?;
    let mut ok = true;

    // 1. Kuunteluosoite jäsentyy.
    match resolve_addr() {
        Ok(addr) => {
            println!("[OK]      addr      {addr}");
            // 2. Portti vapaa — kokeile väliaikaista sidontaa.
            match TcpListener::bind(addr).await {
                Ok(listener) => {
                    println!("[OK]      port      {addr} bindable");
                    drop(listener);
                }
                Err(e) => {
                    println!("[FAIL]    port      {addr} not bindable: {e}");
                    ok = false;
                }
            }
        }
        Err(e) => {
            println!("[FAIL]    addr      {e}");
            ok = false;
        }
    }

    // 3. Vaaditut env-muuttujat — vain läsnäolo, ei arvoja.
    //    (TELEGRAM_TOKEN on salaisuus → ehdottomasti vain set/MISSING.)
    let channel_kind = cfg.channel_kind().to_string();
    let channel_keys: &[&str] = if channel_kind == "discord" {
        &[
            DISCORD_WEBHOOK_URL_ENV,
            DISCORD_CHANNEL_ID_ENV,
            REPLY_TARGET_ENV,
        ]
    } else {
        &[
            TELEGRAM_TOKEN_ENV,
            TELEGRAM_CHANNEL_ID_ENV,
            REPLY_TARGET_ENV,
        ]
    };
    println!("[INFO]     channel   {channel_kind}");
    for key in channel_keys {
        if std::env::var_os(key).is_some_and(|v| !v.is_empty()) {
            println!("[OK]      env       {key} set");
        } else {
            println!("[MISSING] env       {key}");
            ok = false;
        }
    }

    if channel_kind == "discord" {
        if std::env::var_os(DISCORD_PUBLIC_KEY_ENV).is_some_and(|v| !v.is_empty()) {
            println!("[OK]      env       {DISCORD_PUBLIC_KEY_ENV} set (interactions)");
        } else {
            println!(
                "[WARN]    env       {DISCORD_PUBLIC_KEY_ENV} unset — /discord/interactions off"
            );
        }
    }

    if std::env::var_os("FAMILYCLAW_DATA_DIR").is_some_and(|v| !v.is_empty()) {
        println!("[OK]      env       FAMILYCLAW_DATA_DIR set");
    } else {
        println!("[WARN]    env       FAMILYCLAW_DATA_DIR unset — in-memory memory only");
    }

    if std::env::var_os("FAMILYCLAW_PROFILE_DIR").is_some_and(|v| !v.is_empty()) {
        println!("[OK]      env       FAMILYCLAW_PROFILE_DIR set");
    } else {
        println!("[WARN]    env       FAMILYCLAW_PROFILE_DIR unset — generic soul");
    }

    // /inject-suojaus: valinnainen, joten ei kaada doctoria. Vain läsnäolo —
    // token on salaisuus, arvoa ei tulosteta. Puuttuva = varoitus avoimesta
    // endpointista, ei virhe.
    if cfg.gateway_token().trim().is_empty() {
        println!(
            "[WARN]    inject    {GATEWAY_TOKEN_ENV} unset — POST /inject open (loopback-only)"
        );
    } else {
        println!("[OK]      inject    {GATEWAY_TOKEN_ENV} set — POST /inject requires bearer");
    }

    if ok {
        println!("doctor: ok");
        Ok(())
    } else {
        Err(FamilyClawError::invalid_input(
            "doctor: one or more checks failed",
        ))
    }
}

/// Jäsentää [`PLAN_ENV`]-suunnitelman tai palauttaa savutesti-oletuksen.
///
/// JSON-muoto on tarkoituksella pelkistetty: lista solmuja, joista kukin saa
/// `id`/`title`/`description` ja valinnaisen `input`-objektin. Riippuvuudet,
/// roolit ja kyvyt jätetään oletusarvoihin (yksinkertainen lineaarinen ajo),
/// jotta sisäänkäynti pysyy ohuena — monimutkaisempi suunnittelu kuuluu
/// kirjasto-API:lle ([`OrchestrationPlan`]).
fn load_orchestration_plan() -> OrchestrationPlan {
    let raw = std::env::var(PLAN_ENV).unwrap_or_default();
    if raw.trim().is_empty() {
        // Sisäänrakennettu savutesti: yksi solmu joka todistaa että ajo kulkee
        // worker-valinnan + LiveTurnExecutorin läpi.
        return OrchestrationPlan::new(
            "smoke",
            vec![TaskNode::new(
                "n1",
                "smoke turn",
                "Produce a tiny JSON object proving the live orchestration path works.",
            )],
        );
    }
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) => {
            let plan_id = v.get("id").and_then(|x| x.as_str()).unwrap_or("plan");
            let nodes: Vec<TaskNode> = v
                .get("nodes")
                .and_then(|n| n.as_array())
                .map(|arr| {
                    arr.iter()
                        .enumerate()
                        .map(|(i, node)| {
                            let id = node
                                .get("id")
                                .and_then(|x| x.as_str())
                                .map_or_else(|| format!("n{i}"), ToString::to_string);
                            let title = node
                                .get("title")
                                .and_then(|x| x.as_str())
                                .unwrap_or("turn")
                                .to_string();
                            let desc = node
                                .get("description")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            TaskNode::new(id, title, desc)
                        })
                        .collect()
                })
                .unwrap_or_default();
            OrchestrationPlan::new(plan_id, nodes)
        }
        Err(e) => {
            warn!(error = %e, "kelvoton {PLAN_ENV} JSON — käytetään savutesti-oletusta");
            OrchestrationPlan::new(
                "smoke",
                vec![TaskNode::new(
                    "n1",
                    "smoke turn",
                    "fallback after invalid plan",
                )],
            )
        }
    }
}

/// Ajaa monivaiheisen orkesterointisuunnitelman kerran ja tulostaa raportin.
///
/// Kokoaa bridgen, rekisteröi yhden Executor-työntekijän (online heartbeatilla),
/// rakentaa [`LiveTurnExecutor`]:n env-resolverista ja ajaa
/// [`Orchestrator::run_with`]:n. Tulostaa `RunReport`:n JSON-muodossa.
///
/// # Errors
/// [`FamilyClawError`] jos mallin ratkaisu, työntekijän rekisteröinti tai ajo
/// epäonnistuu.
async fn orchestrate() -> Result<()> {
    let cfg = FamilyConfig::load()?;
    let model = cfg.model().to_string();
    info!(%model, "orchestrate: kootaan bridge + LiveTurnExecutor");

    // 1. Bridge-substraatti (oma EventBus/AgentRegistry/TaskBoard).
    let bridge = FamilyBridge::new();
    let now = familyclaw_core::time::now();

    // 2. Rekisteröi yksi Executor-työntekijä ja tee siitä online (heartbeat),
    //    jotta select_worker näkee sen. Geneerinen nimi (KERROS A).
    let worker_id = familyclaw_core::AgentId::new();
    let worker = AgentInfo::new(worker_id, "worker-a", AgentRole::Executor, HostKind::Local);
    bridge.register_agent(worker).await.map_err(|e| {
        FamilyClawError::invalid_input(format!("orchestrate: register failed: {e}"))
    })?;
    bridge.heartbeat(worker_id, now).await.map_err(|e| {
        FamilyClawError::invalid_input(format!("orchestrate: heartbeat failed: {e}"))
    })?;

    // 3. LiveTurnExecutor oikealla LLM-ketjulla (sama resolver kuin serve).
    let resolver = build_resolver();
    let executor = LiveTurnExecutor::from_model(&ModelConfig::new(&model), &resolver)?;
    info!(primary = %executor.primary_model(), "LiveTurnExecutor valmis");

    // 4. Aja suunnitelma.
    let plan = load_orchestration_plan();
    let orchestrator = Orchestrator::new(bridge);
    let report = orchestrator.run_with(&plan, now, &executor).await?;

    // 5. Raportti stdoutiin. RunReport ei johda Serializea (bridge-tyyppi,
    //    jota emme muuta cross-crate), joten käytetään Debug-tulostusta +
    //    pieni JSON-yhteenveto valmistuneista solmuista.
    println!("{report:#?}");
    info!(
        plan = %report.plan_id,
        "orchestrate: valmis"
    );
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
        let not_ready = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
        });
        let (status, _) = readyz(State(not_ready)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);

        // Busin kanssa: valmis (200).
        let bus = ResonanceBus::start(None).await.expect("bus");
        let ready = Arc::new(GatewayState {
            bus: Some(bus.clone()),
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
        });
        let (status, _) = readyz(State(ready)).await;
        assert_eq!(status, StatusCode::OK);
        bus.stop();
    }

    #[test]
    fn build_router_constructs_without_panic() {
        // Reititin rakentuu (tyyppitason savutesti) molemmilla tiloilla.
        let _ = build_router(Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
        }));
    }

    #[test]
    fn cli_definition_is_valid() {
        // clap-määrittely on sisäisesti ehjä (paljastaa derive-virheet).
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn cli_no_args_defaults_to_serve() {
        // Ilman alikomentoa = serve (taaksepäinyhteensopivuus).
        let cli = Cli::parse_from(["familyclaw-gateway"]);
        assert!(
            matches!(cli.command.unwrap_or(Command::Serve), Command::Serve),
            "argumentiton kutsu pitää tarkoittaa serve"
        );
    }

    #[test]
    fn cli_parses_each_subcommand() {
        // serve/status/doctor jäsentyvät odotetuiksi varianteiksi.
        let serve = Cli::parse_from(["familyclaw-gateway", "serve"]);
        assert!(matches!(serve.command, Some(Command::Serve)));

        let status = Cli::parse_from(["familyclaw-gateway", "status"]);
        assert!(matches!(status.command, Some(Command::Status)));

        let doctor = Cli::parse_from(["familyclaw-gateway", "doctor"]);
        assert!(matches!(doctor.command, Some(Command::Doctor)));

        let orch = Cli::parse_from(["familyclaw-gateway", "orchestrate"]);
        assert!(matches!(orch.command, Some(Command::Orchestrate)));
    }

    #[test]
    fn plan_load_falls_back_to_smoke_without_env() {
        // Ilman PLAN_ENV:iä → sisäänrakennettu yhden solmun savutesti.
        // (Testi ei aseta env-muuttujaa, joten luetaan oletus.)
        std::env::remove_var(PLAN_ENV);
        let plan = load_orchestration_plan();
        assert_eq!(plan.id, "smoke");
        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(plan.nodes[0].id.as_str(), "n1");
    }

    #[test]
    fn plan_load_parses_json_nodes() {
        let json = r#"{"id":"p","nodes":[
            {"id":"a","title":"A","description":"da"},
            {"id":"b","title":"B","description":"db"}
        ]}"#;
        std::env::set_var(PLAN_ENV, json);
        let plan = load_orchestration_plan();
        std::env::remove_var(PLAN_ENV);
        assert_eq!(plan.id, "p");
        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(plan.nodes[1].id.as_str(), "b");
        assert_eq!(plan.nodes[1].title, "B");
    }

    #[test]
    fn cli_rejects_unknown_subcommand() {
        // Tuntematon alikomento ei jäsenny (clap palauttaa virheen).
        assert!(Cli::try_parse_from(["familyclaw-gateway", "bogus"]).is_err());
    }

    #[test]
    fn health_url_builds_http_scheme() {
        // status-apuri muodostaa http-URL:n oikein osoitteesta + polusta.
        let addr: SocketAddr = "127.0.0.1:8787".parse().expect("addr");
        assert_eq!(
            health_url(addr, "/healthz"),
            "http://127.0.0.1:8787/healthz"
        );
        assert_eq!(health_url(addr, "/readyz"), "http://127.0.0.1:8787/readyz");
    }

    #[test]
    fn constant_time_eq_matches_only_identical_bytes() {
        // Vakioaikainen vertailu täsmää vain samanpituisiin, tavuittain
        // identtisiin jonoihin (ei oikosulkua ensimmäisestä erosta).
        assert!(constant_time_eq(b"s3cret", b"s3cret"));
        assert!(!constant_time_eq(b"s3cret", b"s3crXt"));
        assert!(!constant_time_eq(b"s3cret", b"s3cre")); // eri pituus
        assert!(constant_time_eq(b"", b""));
    }

    /// Apuri: rakentaa `Authorization`-otsikon sisältävän [`HeaderMap`]:n.
    fn headers_with_auth(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            value.parse().expect("valid header value"),
        );
        h
    }

    #[test]
    fn inject_auth_no_token_configured_accepts() {
        // (c) Tokenia ei konfiguroitu → pyyntö hyväksytään ilman otsikkoa
        //     (taaksepäinyhteensopiva avoin loopback-oletus).
        let state = GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
        };
        assert!(check_inject_auth(&state, &HeaderMap::new()).is_ok());
        // Ylimääräinen otsikko ei haittaa kun suojausta ei ole.
        assert!(check_inject_auth(&state, &headers_with_auth("Bearer whatever")).is_ok());
    }

    #[test]
    fn inject_auth_token_configured_correct_bearer_accepts() {
        // (a) Token konfiguroitu + oikea Bearer → hyväksytään.
        let state = GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: Some(Arc::from("s3cret-token")),
            discord_public_key: None,
            actions: None,
            turn_audit: None,
        };
        assert!(check_inject_auth(&state, &headers_with_auth("Bearer s3cret-token")).is_ok());
    }

    #[test]
    fn inject_auth_token_configured_wrong_or_missing_rejects_401() {
        // (b) Token konfiguroitu + väärä/puuttuva Bearer → 401.
        let state = GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: Some(Arc::from("s3cret-token")),
            discord_public_key: None,
            actions: None,
            turn_audit: None,
        };
        // Väärä token.
        assert_eq!(
            check_inject_auth(&state, &headers_with_auth("Bearer wrong-token")),
            Err(StatusCode::UNAUTHORIZED)
        );
        // Otsikko kokonaan puuttuu.
        assert_eq!(
            check_inject_auth(&state, &HeaderMap::new()),
            Err(StatusCode::UNAUTHORIZED)
        );
        // Bearer-prefiksi puuttuu (paljas token).
        assert_eq!(
            check_inject_auth(&state, &headers_with_auth("s3cret-token")),
            Err(StatusCode::UNAUTHORIZED)
        );
        // Oikea prefiksi mutta tyhjä token.
        assert_eq!(
            check_inject_auth(&state, &headers_with_auth("Bearer ")),
            Err(StatusCode::UNAUTHORIZED)
        );
    }

    // ---- Operaattorin hyväksyntäpinta (suspend/resume-silta, roadmap §6 D2) ----

    /// Apuri: gateway-tila jossa on **kytketty** toimintoajoympäristö (oletustaidot)
    /// eikä bearer-suojausta. Palauttaa myös jaetun kahvan tehtävien lähetykseen.
    fn state_with_actions() -> (Arc<GatewayState>, Arc<Mutex<ActionRuntime>>) {
        let rt = ActionRuntime::with_default_skills().expect("default skills");
        let actions = Arc::new(Mutex::new(rt));
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: Some(Arc::clone(&actions)),
            turn_audit: None,
        });
        (state, actions)
    }

    /// Apuri: lähettää write-external-tehtävän → odottava hyväksyntä syntyy.
    /// Palauttaa myönnetyn hyväksynnän tunnisteen merkkijonona (route-muoto).
    async fn submit_pending(actions: &Arc<Mutex<ActionRuntime>>) -> String {
        use familyclaw_actions::GithubIssueDraftMock;
        let now = familyclaw_core::time::now();
        let mut rt = actions.lock().await;
        let submitted = rt
            .submit_task(
                GithubIssueDraftMock::skill_id(),
                serde_json::json!({ "bug_report": "Button does nothing" }),
                now,
            )
            .await
            .expect("submit");
        submitted
            .pending_approval
            .expect("write-external requires approval")
            .to_string()
    }

    /// Apuri: lähettää odottavan hyväksynnän **injektoidulla `now`-hetkellä**,
    /// jotta vanhentumisraja saadaan testissä determinismillä haltuun.
    ///
    /// Hyväksynnän `expires_at` lasketaan `now + TTL`:nä lähetyshetkellä, joten
    /// kaukana menneisyydessä oleva `now` tuottaa hyväksynnän joka on jo
    /// vanhentunut suhteessa todelliseen nykyhetkeen — juuri tällä `approve`
    /// päätyy `410 Gone` -haaraan ilman kelloa väärentäviä globaaleja tiloja.
    async fn submit_pending_at(
        actions: &Arc<Mutex<ActionRuntime>>,
        now: familyclaw_core::time::Timestamp,
    ) -> String {
        use familyclaw_actions::GithubIssueDraftMock;
        let mut rt = actions.lock().await;
        let submitted = rt
            .submit_task(
                GithubIssueDraftMock::skill_id(),
                serde_json::json!({ "bug_report": "Button does nothing" }),
                now,
            )
            .await
            .expect("submit");
        submitted
            .pending_approval
            .expect("write-external requires approval")
            .to_string()
    }

    #[tokio::test]
    async fn pending_route_503_without_action_runtime() {
        // Ilman kytkettyä toimintoajoympäristöä → 503 (ei paniikkia).
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
        });
        let (status, _) = list_pending_approvals(State(state), HeaderMap::new()).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn pending_route_lists_redacted_without_payload() {
        let (state, actions) = state_with_actions();
        submit_pending(&actions).await;

        let (status, Json(body)) = list_pending_approvals(State(state), HeaderMap::new()).await;
        assert_eq!(status, StatusCode::OK);
        let arr = body.as_array().expect("array body");
        assert_eq!(arr.len(), 1, "yksi odottava hyväksyntä");
        let item = &arr[0];
        // Vain kolme salaisuudetonta kenttää.
        assert!(item.get("approval_id").and_then(|v| v.as_str()).is_some());
        assert!(item.get("redacted_summary").and_then(|v| v.as_str()).is_some());
        assert!(item.get("created_at").and_then(|v| v.as_str()).is_some());
        // EI raakaa payloadia ("bug_report"/"Button does nothing") eikä payload-kenttää.
        let rendered = serde_json::to_string(&body).expect("serialize");
        assert!(!rendered.contains("bug_report"));
        assert!(!rendered.contains("Button does nothing"));
        assert!(!rendered.contains("payload"));
    }

    #[tokio::test]
    async fn pending_route_requires_bearer_when_configured() {
        // Token konfiguroitu mutta ei otsikkoa → 401, ei vuoda listaa.
        let (mut_state, actions) = state_with_actions();
        submit_pending(&actions).await;
        // Rakenna uusi tila samalla runtimella mutta token päällä.
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: Some(Arc::from("s3cret-token")),
            discord_public_key: None,
            actions: mut_state.actions.clone(),
            turn_audit: None,
        });
        let (status, _) = list_pending_approvals(State(state), HeaderMap::new()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn approve_route_invalid_id_is_400() {
        let (state, _actions) = state_with_actions();
        let (status, _) = approve_pending(
            State(state),
            HeaderMap::new(),
            Path("not-a-uuid".to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn approve_route_unknown_id_is_404() {
        let (state, _actions) = state_with_actions();
        // Kelvollinen UUID mutta ei odottavaa hyväksyntää → 404 (fail-closed).
        let unknown = ApprovalId::new().to_string();
        let (status, _) =
            approve_pending(State(state), HeaderMap::new(), Path(unknown)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn approve_route_expired_id_is_410() {
        // Vanhentunut hyväksyntä → 410 Gone (eri syy kuin tuntematon = 404).
        // Lähetetään odottava hyväksyntä kaukana menneisyydessä olevalla
        // `now`-hetkellä (epoch), jolloin `expires_at = epoch + TTL` on jo
        // todellisen nykyhetken takana. `approve_pending` lukee oikean
        // `familyclaw_core::time::now()`:n → `now > expires_at` → 410, ilman
        // että hyväksyntää kulutetaan (fail-closed, ei sivuvaikutusta).
        let (state, actions) = state_with_actions();
        let past = familyclaw_core::time::from_unix_secs(0).expect("epoch is a valid timestamp");
        let id = submit_pending_at(&actions, past).await;

        let (status, _) =
            approve_pending(State(state), HeaderMap::new(), Path(id)).await;
        assert_eq!(status, StatusCode::GONE);
    }

    #[tokio::test]
    async fn approve_route_503_without_action_runtime() {
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
        });
        let (status, _) = approve_pending(
            State(state),
            HeaderMap::new(),
            Path(ApprovalId::new().to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn approve_route_grants_and_runs_to_completion() {
        // Täysi polku: submit → odottava → approve → keskeytynyt toiminto ajetaan
        // loppuun (status done), ei vaadi uutta hyväksyntää.
        let (state, actions) = state_with_actions();
        let id = submit_pending(&actions).await;

        let (status, Json(body)) =
            approve_pending(State(Arc::clone(&state)), HeaderMap::new(), Path(id.clone())).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.get("status").and_then(|v| v.as_str()), Some("done"));
        assert_eq!(
            body.get("awaiting_further_approval")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );

        // Hyväksyntä kulutettu → ei enää odottavissa, ja toinen approve = 404.
        let (status2, _) = approve_pending(State(state), HeaderMap::new(), Path(id)).await;
        assert_eq!(status2, StatusCode::NOT_FOUND, "kertakäyttö: ei voi hyväksyä uudelleen");
    }
}
