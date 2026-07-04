//! # familyclaw-gateway
//!
//! **Gateway-binääri** — FamilyClaw-alustan (KERROS A, OSS) pitkäikäinen
//! prosessi: se sitoo HTTP-portin, tarjoaa elinvoima- ja valmiustarkistukset
//! (`/healthz`, `/readyz`) sekä Prometheus-mittarit (`/metrics`), käynnistää
//! [`FamilyRuntime`]:n (bus + agentti + kanava + reply-pumppu) yhdellä
//! [`build_family`]-kutsulla ja pysyy pystyssä kunnes käyttäjä pyytää siistin
//! sammutuksen (`Ctrl-C`).
//!
//! ## Havainnoitavuus: `GET /metrics` (Prometheus-eksposition tekstiformaatti)
//! Gateway jakaa [`MetricsRegistry`]:n (rakennettu
//! [`MetricsRegistry::with_fleet_defaults`]:lla) `GatewayState`-tilaansa ja
//! tarjoilee sen `GET /metrics`:llä `text/plain`-vastauksena
//! ([`MetricsRegistry::prometheus_export`], deterministinen nimijärjestys).
//! Laivueen esinimetyt sarjat (luodut/valmistuneet tehtävät, sopimukset,
//! agenttivuorot, LLM-kutsut, `agents_online`-gauge, …) ovat viennissä alusta
//! asti arvolla `0`. **Tapahtumapohjainen täyttö on KYTKETTY:**
//! [`serve`] tilaa siltakerroksen tapahtumaväylän
//! ([`FamilyBridge`]) [`EventRecorder`]illa
//! ja antaa SAMAN [`MetricsRegistry`]:n recorderille ja `GatewayState`:lle.
//! Ajonaikaiset tapahtumat inkrementoivat siis ne sarjat jotka recorder
//! kartoittaa: agentin rekisteröinti nostaa `agents_online`-gaugea heti
//! käynnistyksessä, ja siltakerroksen tehtävä-/sopimus-/LLM-tapahtumat
//! (`task.*`, `contract.*`, `llm.*`, `agent.turn`, `workflow.*`) kasvattavat
//! vastaavia laskureita. Sarjat joille ei tuoteta tapahtumaa pysyvät nollassa.
//! Reitti on suojaamaton — mittarit ovat numeerisia aikasarjoja ilman
//! salaisuuksia (ks. [`metrics_handler`]).
//!
//! ```bash
//! curl -s http://127.0.0.1:8787/metrics
//! # → # TYPE agents_online gauge
//! #   agents_online 1          # agentti rekisteröitiin käynnistyksessä
//! #   # TYPE tasks_created counter
//! #   tasks_created 0          # nousee kun siltakerros luo tehtäviä
//! #   ...
//! ```
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
use familyclaw_bus::{BeingId, BusHandle, BusMessage};
use tokio::sync::Mutex;
mod config;
use config::FamilyConfig;
use familyclaw_channels::{
    verify_signature, Channel, ChannelKind, ChannelResult, DiscordChannel, DiscordInteraction,
    InboundMessage, MessageStream, OutboundMessage, SendFuture, TelegramChannel,
    RESPONSE_DEFERRED_CHANNEL_MESSAGE, RESPONSE_PONG,
};
use familyclaw_core::{AgentConfig, FamilyClawError, ModelConfig, Result};
use familyclaw_observability::{EventRecorder, MetricsRegistry};
use familyclaw_runtime::{build_family, FamilyRuntime};
use familyclaw_scheduler::{AgencyConfig, ScheduledTaskId, SchedulerHandle};
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
const DISCORD_BOT_TOKEN_ENV: &str = "DISCORD_BOT_TOKEN";
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

/// Valinnainen LLM-output-katto (tokeneina). Ilman tätä LlmConfig-oletus on
/// 2048, joka katkaisee pitkät vastaukset kesken lauseen. Aseta esim. 8192
/// jotta agentti (esim. agent_delta tutkimusraportit) mahtuu vastaamaan kokonaan.
const MAX_TOKENS_ENV: &str = "FAMILYCLAW_MAX_TOKENS";

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
    /// **Jaettu ajastinkahva** perhe-agency-operaattoripinnalle
    /// (`POST /tasks/{id}/enabled`, Phase 4 kill-switch).
    ///
    /// Sama [`SchedulerHandle`] jonka [`FamilyRuntime`] altistaa
    /// ([`FamilyRuntime::scheduler_handle`]) — operaattori voi kytkeä ajastettuja
    /// tehtäviä päälle/pois saman lukon kautta jota tikkisilmukka käyttää.
    /// `Some` palvelevassa gatewayssa kun ajastin on käynnissä; `None` kun
    /// ajastin on pois käytöstä (`FAMILYCLAW_DREAM_DISABLED`) tai runtimea ei
    /// ole kytketty. Kun `None`, kill-switch-reitti vastaa `503`.
    scheduler: Option<SchedulerHandle>,
    /// Perhe-agency-configin polku (`<data_dir>/agency.json`) johon kill-switch-
    /// muutos persistoidaan (Phase 4). `Some` kun ajastin pyörii persistentillä
    /// polulla; `None` muistinvaraisessa tilassa → muutos jää vain muistiin
    /// (häviää restartissa, mikä on oikein in-memory-tilalle).
    agency_config_path: Option<std::path::PathBuf>,
    /// **Jaettu mittarirekisteri** Prometheus-viennille (`GET /metrics`).
    ///
    /// [`MetricsRegistry`] on `Clone` ja jakaa tilansa `Arc`:n kautta, joten
    /// tämä kahva näkee tarkalleen samat mittarit kuin se instanssi joka
    /// kasvattaa niitä. [`serve`] antaa TÄSMÄLLEEN saman rekisterin myös
    /// [`EventRecorder`]ille (joka
    /// tilaa siltakerroksen tapahtumaväylän), joten ajonaikaiset tapahtumat
    /// inkrementoivat näitä sarjoja elävästi. Rakennetaan
    /// [`MetricsRegistry::with_fleet_defaults`]:lla, joten laivueen sarjat
    /// (esim. luodut tehtävät, online olevat agentit) ovat viennissä alusta asti
    /// arvolla `0` — dashboardit eivät "katoa" ennen ensimmäistä tapahtumaa.
    ///
    /// Vienti on aina turvallinen: [`MetricsRegistry::prometheus_export`]
    /// palauttaa pelkän `String`:n eikä koskaan vuoda salaisuuksia (mittareilla
    /// on vain numeeriset arvot, ei payloadia). `None` vain tiloissa joissa
    /// rekisteriä ei ole kytketty (esim. osa testeistä).
    metrics: Option<MetricsRegistry>,
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

    info!(
        count = views.len(),
        "approvals: listattiin odottavat hyväksynnät (redaktoituina)"
    );
    let body = serde_json::to_value(&views).unwrap_or_else(|_| serde_json::json!([]));
    (StatusCode::OK, Json(body))
}

/// `POST /approvals/{approval_id}/approve` — **hyväksyy** annetun
/// `approval_id`:n ja **välittää jatkon vuoron OMISTAVALLE agentille** bussin
/// [`BusMessage::ResumeApproval`]-ohjaussignaalilla (suspend/resume-silta,
/// roadmap §6 D2).
///
/// ## Yksi kuluttaja kertakäyttöiselle hyväksynnälle (Option A)
/// Hyväksyntä on **kertakäyttöinen**: sen kuluttaa (ajaa sivuvaikutuksen +
/// poistaa odottavista) tasan yksi taho. Tässä mallissa kuluttaja on
/// **agentti**, ei gateway. Gateway VALIDOI (auth + esitarkistus 400/404/410)
/// ja sitten **julkaisee** `ResumeApproval`-signaalin; vuoron omistava agentti
/// jatkaa [`handle_resume_signal`](familyclaw_agent::Agent::handle_resume_signal)
/// → [`resume_approved`](familyclaw_agent::Agent::resume_approved)-polulle, ajaa
/// payload-sidotun sivuvaikutuksen **TASAN KERRAN** ja reitittää lopullisen
/// vastauksen alkuperäiselle kanavalle — **ilman uutta LLM-vuoroa**. Gateway EI
/// kuluta hyväksyntää (kaksi kuluttajaa yhdelle kertakäyttöiselle hyväksynnälle
/// olisi mahdoton: jälkimmäinen näkisi `ApprovalMissing`).
///
/// ## Asynkroninen semantiikka
/// `200 OK` tarkoittaa **hyväksyntä otettu vastaan ja välitetty omistaja-
/// agentille** — EI että sivuvaikutus on jo ajettu. Sivuvaikutus ajetaan ja
/// vastaus toimitetaan **asynkronisesti** kanavalle (oikea UX). Vastausrunko ei
/// siksi voi sisältää tehtävän lopputulosta; se sisältää vain tunnisteen + tilan
/// `resuming`.
///
/// Rungolla ei ole pakollista sisältöä (valinnainen). Esitarkistus on
/// **READ-ONLY** ([`ActionRuntime::pending_expiry_for`]) eikä kuluta
/// hyväksyntää; payload-sidonta + kertakäyttö tapahtuu agentin
/// [`ActionRuntime::approve`]-kutsussa, joten muutettu runko ei voi käyttää
/// hyväksyntää eikä vuotaa salaisuuksia suoritukseen.
///
/// Suojaus on sama kuin `POST /inject`:llä (bearer-token jos konfiguroitu).
///
/// Tilakoodit (**fail-closed, ei paniikkia**):
/// - `200 OK` + `{ approval_id, status: "resuming", note }` — hyväksyntä
///   otettu vastaan ja välitetty agentille; sivuvaikutus + vastaus asynkronisesti
///   kanavalle,
/// - `400 Bad Request` jos `approval_id` ei jäsenny kelvolliseksi tunnisteeksi,
/// - `401 Unauthorized` jos bearer-token vaaditaan eikä täsmää,
/// - `404 Not Found` jos tunnistetta ei (enää) odota hyväksyntä (tuntematon tai
///   jo kulutettu),
/// - `410 Gone` jos hyväksyntä on vanhentunut (TTL umpeutunut),
/// - `503 Service Unavailable` jos (a) toimintoajoympäristöä ei ole kytketty, (b)
///   bussia ei ole kytketty (Option A vaatii serve-tilan, jossa agentti
///   kuuntelee bussia — ilman bussia jatkoa ei voi koskaan tapahtua), tai (c)
///   signaalin julkaisu epäonnistui. Kaikissa kolmessa tapauksessa hyväksyntää
///   EI kulutettu → pyynnön voi turvallisesti yrittää uudelleen.
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

    // **Read-only esitarkistus** (Option A): erotellaan "tuntematon" (404) ja
    // "vanhentunut" (410) ennen kuin välitämme jatkon agentille. Tämä EI kuluta
    // hyväksyntää ([`ActionRuntime::pending_expiry_for`] on read-only) — kuluttaja
    // on agentti. Ilman tätä erottelua 404 ja 410 näyttäisivät samalta agentin
    // resume-polulla, emmekä voisi antaa operaattorille fail-closed-tarkkaa syytä.
    let rt = actions.lock().await;
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

    // Esitarkistus läpäisty: hyväksyntä on olemassa eikä vanhentunut. EMME
    // kuluta sitä gatewayssä (Option A) — vapauta toiminto-lukko ja siirrä
    // jatko vuoron omistavalle agentille bussin kautta. Vain agentti kuluttaa
    // kertakäyttöisen hyväksynnän (ajaa sivuvaikutuksen + reitittää vastauksen),
    // joten emme pidä lukkoa kun julkaisemme.
    drop(rt);

    // **Resume-silta (Phase 1 §6 manuaaliportti, Option A):** julkaise
    // ohjaussignaali `ResumeApproval` bussiin, jotta vuoron OMISTAVA agentti
    // jatkaa `handle_resume_signal` → `resume_approved` → `route_reply`-polulle
    // (kuluttaa hyväksynnän, ajaa sivuvaikutuksen TASAN KERRAN, reitittää
    // lopullisen vastauksen kanavaan) ILMAN uutta LLM-vuoroa.
    //
    // `publish` on **broadcast** (ei point-to-point) — ja se on tässä
    // turvallista: vain omistava agentti kuluttaa resumen (omistajatarkistus
    // `resume_approved`:ssa epäonnistuu suljettuna muille), joten muut olennot
    // no-oppaavat. `from`-tunniste vaikuttaa vain itse-kaiun ohitukseen; gateway
    // ei ole rekisteröity olento, joten tuore `BeingId::new()` riittää (ei voi
    // osua kehenkään).
    let Some(bus) = state.bus.as_ref() else {
        // **Ei bussia → 503, EI hiljaista onnistumista.** Option A:ssa
        // sivuvaikutus ajetaan VAIN agentin resume-polulla; ilman bussia
        // (esim. CLI / ei-serve-konteksti) yhtään agenttia ei kuuntele, joten
        // jatkoa ei voi koskaan tapahtua. Hyväksyntää EI kulutettu → rehellinen
        // 503: operaattori-approve vaatii serve-tilan (bussilla ajava agentti).
        warn!(
            approval = %id,
            "approvals: approve hylätty 503 — bussia ei kytketty (Option A vaatii serve-tilan, \
             jossa omistava agentti kuuntelee bussia); hyväksyntää ei kulutettu, voi yrittää uudelleen"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "operator approve requires serve mode (a running agent on the bus); \
                          approval was not actioned and can be retried"
            })),
        );
    };

    let signal = BusMessage::ResumeApproval {
        approval_id: approval_id.clone(),
    };
    if let Err(e) = bus.publish(BeingId::new(), signal) {
        // **Julkaisu epäonnistui → 503, hyväksyntää EI kulutettu.** Jos emme voi
        // ilmoittaa agentille, jatkoa ei tapahdu. Älä palauta valheellista 200:aa
        // — palauta rehellinen 503 (yhä odottava, voi yrittää uudelleen).
        warn!(
            approval = %id,
            error = %e,
            "approvals: approve hylätty 503 — ResumeApproval-signaalin julkaisu epäonnistui; \
             hyväksyntää ei kulutettu, voi yrittää uudelleen"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "could not notify the owning agent (bus publish failed); \
                          approval was not actioned and can be retried"
            })),
        );
    }

    info!(
        approval = %id,
        "approvals: hyväksyntä otettu vastaan ja ResumeApproval julkaistu — omistava agentti \
         ajaa sivuvaikutuksen + vastaa kanavalle asynkronisesti"
    );
    // **200 = hyväksyntä otettu vastaan ja välitetty agentille.** EI lopputulosta:
    // sivuvaikutus + vastaus ajetaan asynkronisesti agentin resume-polulla, joten
    // emme palauta task_id/status-pakettia (ei payloadia, ei salaisuuksia).
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "approval_id": approval_id,
            "status": "resuming",
            "note": "agent is completing the approved action; the reply will arrive on the originating channel"
        })),
    )
}

/// `POST /tasks/{task_id}/enabled` — **perhe-agency kill-switch** (Phase 4):
/// kytkee ajastetun tehtävän päälle tai pois.
///
/// Runko: JSON `{"enabled": true|false}`. `enabled=false` = ajastin ohittaa
/// tehtävän seuraavissa tikeissä (kill-switch); `true` = ottaa taas käyttöön.
/// Mutaatio menee saman lukon kautta jota tikkisilmukka käyttää, joten kilpailu
/// ratkeaa lukolla.
///
/// Vastaa:
/// - `200 OK` + uusi tila kun tehtävä löytyi ja kytkettiin,
/// - `400 Bad Request` jos tunniste on epäkelpo UUID tai runko puuttuu `enabled`,
/// - `401` jos bearer-auth vaaditaan eikä täsmää,
/// - `404 Not Found` jos tunnistetta ei ole rekisteröity ajastimeen,
/// - `503 Service Unavailable` jos ajastin ei ole kytketty (esim. dream pois
///   käytöstä tai runtimea ei ole).
async fn set_task_enabled_route(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    if check_inject_auth(&state, &headers).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        );
    }
    let Some(scheduler) = state.scheduler.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "scheduler not configured" })),
        );
    };
    let Some(enabled) = payload.get("enabled").and_then(serde_json::Value::as_bool) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "body must be {\"enabled\": bool}" })),
        );
    };
    let Ok(uuid) = uuid::Uuid::parse_str(task_id.trim()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid task id" })),
        );
    };
    let id = ScheduledTaskId::from_uuid(uuid);

    let mut sched = scheduler.lock().await;
    if sched.set_task_enabled(id, enabled) {
        drop(sched); // vapauta lukko ennen tiedosto-I/O:ta
                     // Persistoi muutos config-tiedostoon, jotta kill-switch
                     // säilyy yli restartin (Phase 4). Best-effort: persistenssin
                     // epäonnistuminen ei kumoa live-muutosta (joka jo tehtiin),
                     // mutta se lokitetaan — muistinvaraisessa tilassa polkua ei
                     // ole, jolloin muutos jää vain muistiin (oikein in-memorylle).
        if let Some(path) = state.agency_config_path.as_ref() {
            match AgencyConfig::load(path) {
                Ok(mut cfg) => {
                    cfg.set(id, enabled);
                    if let Err(e) = cfg.save(path) {
                        tracing::warn!(target: "familyclaw::scheduler", error = %e, "failed to persist agency config — live change kept, restart may revert it");
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "familyclaw::scheduler", error = %e, "failed to load agency config for persist — live change kept");
                }
            }
        }
        (
            StatusCode::OK,
            Json(serde_json::json!({ "task_id": task_id, "enabled": enabled })),
        )
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": "no such scheduled task" })),
        )
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
    info!(
        count = events.len(),
        "turns: listattiin redaktoitu tool-loop-audit-jälki"
    );
    let body = serde_json::to_value(&events).unwrap_or_else(|_| serde_json::json!([]));
    (StatusCode::OK, Json(body))
}

/// Prometheus-vastauksen sisältötyyppi (eksposition tekstiformaatti).
///
/// Käytämme `version=0.0.4`-eksposition vakiotyyppiä (`text/plain`), jonka
/// Prometheus-keräin ymmärtää suoraan. Charset on `utf-8` (mittarinimet ovat
/// ASCII, mutta eksplisiittinen charset on eksposition suositus).
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// `GET /metrics` — vie jaetun [`MetricsRegistry`]:n **deterministisessä**
/// Prometheus-eksposition tekstiformaatissa (`prometheus_export`).
///
/// Vastauksen sisältötyyppi on [`PROMETHEUS_CONTENT_TYPE`] (`text/plain`),
/// jonka Prometheus-keräin osaa jäsentää. Runko järjestetään mittarinimen mukaan
/// ([`MetricsRegistry`] perustuu `BTreeMap`:iin), joten tuloste on vakaa eikä
/// vaihtele pyyntöjen välillä — sama panos tuottaa saman tulosteen.
///
/// **Mitkä mittarit ovat "eläviä" — tarkka, rehellinen tila:** rekisteri
/// rakennetaan [`MetricsRegistry::with_fleet_defaults`]:lla, joten kaikki
/// laivueen esinimetyt laskurit ja `agents_online`-gauge ovat viennissä alusta
/// asti (arvolla `0`). [`serve`] tilaa siltakerroksen tapahtumaväylän
/// [`EventRecorder`]illa ja antaa SAMAN rekisterin sekä recorderille että tälle
/// handlerille — joten **mekanismi** (tapahtuma → laskurin inkrementti →
/// `/metrics`) on kytketty ja e2e-testattu.
///
/// **MUTTA tuotannon ajavassa gatewayssä vain YKSI sarja todella liikkuu tällä
/// hetkellä:**
/// - ✅ `agents_online` (gauge) — `build_family` julkaisee `AgentRegistered`:n
///   käynnistyksessä tarjoiltuun väylään → `1`.
/// - ⏳ `tasks_created`, `task_handoffs`, `tasks_completed`, `contract_*`,
///   `agent_turns`, `llm_*`, `durable_replays`, `workflow_steps_completed` ovat
///   **kytketty mutta ruokkimatta** (wired-but-unfed): recorder kartoittaa ne,
///   mutta mikään live-gateway/agentti/orkestrointipolku ei vielä julkaise
///   vastaavia tapahtumia (`TaskCreated` / `Custom("task.completed" |
///   "contract.*" | "llm.*" | …)`) TÄHÄN tarjoiltuun väylään (`orchestrate`
///   käyttää erillistä, kytkemätöntä väylää). Ne pysyvät siis `0`:na kunnes
///   tool-loop/orkestrointi/contract/llm-kerrokset julkaisevat tarjoiltuun
///   väylään — se on seuraava kytkentätyö, ei tämän reitin vika.
///
/// `prometheus_export` palauttaa aina TODELLISET luvut, ei arvauksia — nolla
/// tarkoittaa rehellisesti "ei vielä tapahtumia", ei "rikki".
///
/// Tilakoodit:
/// - `200 OK` + Prometheus-teksti (myös enimmäkseen nollainen runko on validi),
/// - `503 Service Unavailable` jos rekisteriä ei ole kytketty
///   ([`GatewayState::metrics`] = `None`).
///
/// Reitti on **suojaamaton** (ei bearer-tokenia): mittarit ovat numeerisia
/// aikasarjoja ilman salaisuuksia, ja keräimet (Prometheus) eivät tyypillisesti
/// lähetä `Authorization`-otsikkoa. Verkkotason rajaus (loopback-sidonta /
/// palomuuri) on oikea suojakerros tälle endpointille.
async fn metrics_handler(
    State(state): State<Arc<GatewayState>>,
) -> (
    StatusCode,
    [(axum::http::header::HeaderName, &'static str); 1],
    String,
) {
    let content_type = (axum::http::header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE);
    let Some(registry) = state.metrics.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [content_type],
            "# metrics registry not configured\n".to_string(),
        );
    };
    (StatusCode::OK, [content_type], registry.prometheus_export())
}

/// Rakentaa gatewayn HTTP-reitityksen jaetulla tilalla.
fn build_router(state: Arc<GatewayState>) -> Router {
    use axum::routing::post;
    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        // Prometheus-mittarit (jaettu MetricsRegistry, with_fleet_defaults).
        // Rekisteröidään aina; kun rekisteriä ei ole kytketty
        // ([`GatewayState::metrics`] = `None`), handler vastaa 503. Suojaamaton
        // (numeerisia aikasarjoja ilman salaisuuksia) — ks. metrics_handler.
        .route("/metrics", get(metrics_handler))
        .route("/inject", post(inject_discord))
        // Operaattorin hyväksyntäpinta (suspend/resume-silta, roadmap §6 D2).
        // Rekisteröidään aina; kun toimintoajoympäristöä ei ole kytketty
        // ([`GatewayState::actions`] = `None`), handlerit vastaavat 503.
        // Bearer-suojaus on sama kuin /inject:llä (`check_inject_auth`).
        .route("/approvals/pending", get(list_pending_approvals))
        // axum 0.7 (matchit 0.7) käyttää `:param`-syntaksia polkukaappaukseen;
        // `{approval_id}` tulkittaisiin LITERAALIksi segmentiksi → 404 HTTP:n yli.
        .route("/approvals/:approval_id/approve", post(approve_pending))
        // Perhe-agency kill-switch (Phase 4): kytkee ajastetun tehtävän
        // päälle/pois. Rekisteröidään aina; kun ajastinta ei ole kytketty
        // ([`GatewayState::scheduler`] = `None`), handler vastaa 503. Bearer-
        // suojaus on sama kuin /inject:llä.
        .route("/tasks/:task_id/enabled", post(set_task_enabled_route))
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
///
/// **Key-pool (failover gap #1 step 3):** `KEY_ENV`-kenttä voi olla
/// **pilkulla eroteltu lista** env-muuttujia, jolloin avaimet kierrätetään
/// round-robinilla `AuthFailed`-tilanteessa ennen koko providerin jäähdytystä,
/// esim. `openai=https://api.openai.com/v1=OPENAI_API_KEY_1,OPENAI_API_KEY_2`.
/// Yhden avaimen syntaksi (`=OPENAI_API_KEY`) on yhä taaksepäin-yhteensopiva.
///
/// Tyhjä/asettamaton muuttuja → tyhjä resolveri (agentti ajaa ilman LLM:ää).
/// Virheelliset rivit ohitetaan varoituksella — yksi typo ei kaada gatewayta.
fn build_resolver() -> EnvEndpointResolver {
    let mut resolver = EnvEndpointResolver::new();
    // Valinnainen output-token-katto envistä. Sovelletaan KAIKKIIN ratkaistuihin
    // malleihin (apply_tunings). Ilman tätä oletus 2048 katkaisee pitkät vastaukset.
    if let Ok(raw) = std::env::var(MAX_TOKENS_ENV) {
        match raw.trim().parse::<u32>() {
            Ok(max) if max > 0 => {
                resolver = resolver.with_max_tokens(max);
                info!(
                    max_tokens = max,
                    "LLM output-katto asetettu {MAX_TOKENS_ENV}:stä"
                );
            }
            _ => warn!(
                value = raw,
                "ohitetaan kelvoton {MAX_TOKENS_ENV} (odotettu positiivinen kokonaisluku)"
            ),
        }
    }
    let Ok(spec) = std::env::var(PROVIDERS_ENV) else {
        return resolver;
    };
    for entry in spec.split(';').filter(|s| !s.trim().is_empty()) {
        let parts: Vec<&str> = entry.splitn(3, '=').map(str::trim).collect();
        if let [prefix, base_url, key_field] = parts.as_slice() {
            // Avain-kenttä voi olla pilkulla eroteltu pool (round-robin-rotaatio).
            let key_envs: Vec<String> = key_field
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect();
            if !prefix.is_empty() && !base_url.is_empty() && !key_envs.is_empty() {
                resolver = resolver.with_provider_keys(*prefix, *base_url, key_envs);
                continue;
            }
        }
        warn!(
            entry,
            "ohitetaan kelvoton {PROVIDERS_ENV}-rivi (odotettu prefix=base_url=KEY_ENV[,KEY_ENV2])"
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
/// `bridge` on jaettu siltakerroksen tapahtumaväylä havainnoitavuutta varten:
/// se annetaan [`build_family`]:lle, joka julkaisee sille agentin rekisteröinnin
/// (→ `agents_online`-gauge). Kutsujan (serve) on jo tilattava se
/// [`EventRecorder`]illa ennen tätä kutsua, jotta tapahtuma ei huku.
///
/// Resolvoi `/inject`-suojaustokenin konfiguraatiosta. Tyhjä token = avoin
/// loopback-only-oletus (varoitus); asetettu token = pakollinen bearer-täsmäys.
/// Arvoa ei koskaan lokiteta.
fn resolve_inject_token(cfg: &FamilyConfig) -> Option<Arc<str>> {
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
}

/// Palauttaa runtimen, Discord-kanavan (inject/interactions), inject-tokenin ja public keyn.
// Kolme kanavahaaraa (none / discord / telegram), joista kukin kokoaa runtimen
// omalla polullaan — pitkä mutta lineaarinen; jakaminen hämärtäisi luettavuutta.
#[allow(clippy::too_many_lines)]
async fn start_runtime(
    bridge: FamilyBridge,
) -> Result<(
    FamilyRuntime,
    Option<Arc<DiscordChannel>>,
    Option<Arc<str>>,
    Option<Arc<str>>,
)> {
    let cfg = FamilyConfig::load()?;
    let agent_name = cfg.agent_name().to_string();
    let model = cfg.model().to_string();
    let channel_kind = cfg.channel_kind().to_string();

    let inject_token: Option<Arc<str>> = resolve_inject_token(&cfg);

    // KANAVATON JULKAISUTILA (`FAMILYCLAW_CHANNEL_KIND=none`): käynnistä gateway
    // ILMAN yhtään perhe-avainta, -sielua tai reply-kohdetta. Kokoaa runtimen
    // taustalle [`MockChannel`]illä (muistinvarainen, ei ulkoista SDK:ta), joten
    // tuore `cargo install` -käyttäjä voi `serve` + `status`-varmistaa HTTP-pinnan
    // (`/healthz`, `/readyz`, `/metrics`) ENNEN kuin kytkee oikean kanavan. Tämä
    // on julkaistavuuden edellytys: OSS-raja (KERROS A) tarkoittaa että alusta
    // toimii tyhjällä profiililla — Telegram/Discord ovat KERROS B -lisukkeita.
    if channel_kind == "none" {
        info!(
            "kanavaton julkaisutila (FAMILYCLAW_CHANNEL_KIND=none) — MockChannel, ei perhe-avaimia"
        );
        let mock = familyclaw_channels::MockChannel::new("familyclaw-none")
            .map_err(FamilyClawError::from)?;
        let channel: Box<dyn Channel> = Box::new(mock);
        // Reply-kohdetta ei vaadita kanavattomassa tilassa — MockChannel nielee
        // vastaukset outboxiinsa. Käytämme neutraalia paikanpitäjää joka ei
        // reititä minnekään ulos.
        let reply_target = "none".to_string();
        let mut model_cfg = ModelConfig::new(cfg.model().to_string());
        for fb in cfg.fallback_models() {
            model_cfg = model_cfg.with_fallback(fb);
        }
        let agent_cfg = AgentConfig::new_with_stable_id(&agent_name, model_cfg);
        let soul = load_agent_soul(&agent_name);
        let resolver = build_resolver();
        let runtime = build_family(
            Some(DEFAULT_BUS_NAME.to_string()),
            agent_cfg,
            soul,
            channel,
            reply_target,
            &resolver,
            Some(bridge),
        )
        .await?;
        return Ok((runtime, None, inject_token, None));
    }

    let (channel, discord_ch): (Box<dyn Channel>, Option<Arc<DiscordChannel>>) = if channel_kind
        == "discord"
    {
        let bot_token = cfg.discord_bot_token();
        let ch_id = cfg.discord_channel_id();
        // KAKSISUUNTAINEN bot-moodi, jos DISCORD_BOT_TOKEN on asetettu: serenity-
        // gateway kuuntelee (MESSAGE_CONTENT) JA postaa. Muuten fallback
        // yksisuuntaiseen webhook-postaukseen (DISCORD_WEBHOOK_URL).
        // Rakenna DiscordChannel TÄSMÄLLEEN KERRAN ja jaa sama instanssi: bus-pumppu
        // saa `SharedDiscordChannel`-adapterin, inject-polut `Arc`-kahvan — molemmat
        // samaan `inbound_tx`/`inbound_rx`-pariin (ks. SharedDiscordChannel-dokumentaatio).
        let dc = if bot_token.is_empty() {
            let webhook_url = cfg.discord_webhook_url();
            if webhook_url.is_empty() {
                return Err(FamilyClawError::invalid_input(format!(
                        "discord channel requires DISCORD_BOT_TOKEN (kaksisuuntainen) tai {DISCORD_WEBHOOK_URL_ENV} (postaus)"
                    )));
            }
            info!("Discord: yksisuuntainen webhook-postaus");
            DiscordChannel::from_webhook(webhook_url.to_string(), ch_id.to_string())
                .map_err(FamilyClawError::from)?
        } else {
            let cid: u64 = ch_id.trim().parse().map_err(|_| {
                FamilyClawError::invalid_input(format!(
                    "DISCORD_CHANNEL_ID must be a numeric id for bot mode, got: {ch_id:?}"
                ))
            })?;
            // owner_id konfigista (TOML + env FAMILYCLAW_OWNER_ID config-rajalla); 0 = DM:t pois.
            let dc = DiscordChannel::new(bot_token.to_string(), cid, cfg.discord_owner_id())
                .map_err(FamilyClawError::from)?;
            // Käynnistä gateway-yhteys: palaa vasta kun `ready` tai virhe.
            dc.start().await.map_err(FamilyClawError::from)?;
            info!("Discord: kaksisuuntainen bot-moodi (kanava {cid})");
            dc
        };
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
    // Mallikonfiguraatio: primary + valinnaiset varamallit
    // (FAMILYCLAW_FALLBACK_MODELS). Ilman fallbackeja agentti ajaa VAIN
    // primaryllä — jos se on alhaalla/quota loppu, koko olento on hiljaa.
    // LlmFailover (llm_chain.rs) siirtyy seuraavaan kun primary epäonnistuu.
    let mut model_cfg = ModelConfig::new(model);
    let fallbacks = cfg.fallback_models();
    if fallbacks.is_empty() {
        info!(agent = %agent_name, "malli: vain primary (ei FAMILYCLAW_FALLBACK_MODELS)");
    } else {
        info!(agent = %agent_name, count = fallbacks.len(), "malli: primary + varamallit");
        for fb in fallbacks {
            model_cfg = model_cfg.with_fallback(fb);
        }
    }
    let agent_cfg = AgentConfig::new_with_stable_id(&agent_name, model_cfg);
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
        Some(bridge),
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

    // Prometheus-mittarit (GET /metrics): rakennetaan laivueen oletuksilla, ja
    // jaetaan SAMA instanssi sekä havainnoitavuus-tallentimelle (joka kasvattaa
    // sarjoja) että GatewayState:lle (joka tarjoilee ne) — `MetricsRegistry` on
    // `Clone` + `Arc`-jaettu, joten molemmat näkevät samat luvut.
    let metrics = MetricsRegistry::with_fleet_defaults();

    // Havainnoitavuussilta: tilaa siltakerroksen tapahtumaväylä EventRecorderilla
    // ENNEN runtimen kokoamista (EventBus toimittaa vain tilauksen jälkeen
    // julkaistut tapahtumat). Sama `bridge` annetaan build_family:lle, joka
    // julkaisee agentin rekisteröinnin → recorder kasvattaa jaettua rekisteriä
    // (agents_online). Taustatehtävä valuttaa tapahtumat jatkuvasti (run = estävä
    // silmukka kunnes silta sulkeutuu).
    let bridge = FamilyBridge::new();
    let recorder = EventRecorder::new(&bridge, metrics.clone());
    tokio::spawn(recorder.run());

    // C5-sauma: yksi build_family-kutsu kokoaa bus + agentti + kanava +
    // reply-pumppu (FamilyRuntime). Bus-kahva luovutetaan GatewayState:lle;
    // HTTP-/sammutuskuori pysyy ennallaan (vain bus.stop() → runtime.shutdown()).
    // Sama `bridge` viedään runtimeen, joka julkaisee sille agentin
    // rekisteröinnin (EventRecorder jo tilannut yllä).
    let (runtime, discord_ch, inject_token, discord_public_key) = start_runtime(bridge).await?;
    info!("FamilyRuntime käynnissä (bus + agentti + kanava)");

    // Operaattorin hyväksyntäpinta jakaa SAMAN Arc<Mutex<ActionRuntime>>-kahvan
    // jonka build_family kytki agentin tool-looppiin — odottavat hyväksynnät
    // (suspend) ja niiden myöntäminen (resume) tapahtuvat samassa lukitussa
    // tilassa. Vrt. roadmap §6 D2.
    let actions = Some(runtime.actions());
    // Havainnoitava tool-loop-jälki (TURN-AUDIT, roadmap §6 D6): sama
    // Arc<AuditCollector> jonka build_family kytki agentin tool-looppiin.
    let turn_audit = Some(runtime.turn_audit());
    // Ajastinkahva (perhe-agency, Phase 4): sama SchedulerHandle jonka runtime
    // altistaa → kill-switch-reitti kytkee tehtäviä päälle/pois. None jos
    // ajastin ei ole käynnissä.
    let scheduler = runtime.scheduler_handle();
    // Agency-configin polku: kill-switch-muutos persistoidaan tähän (Phase 4).
    let agency_config_path = runtime.agency_config_path();

    // Mittarirekisteri (GET /metrics): SAMA instanssi jonka EventRecorder yllä
    // sai (metrics.clone()). Tapahtumapohjainen täyttö on nyt KYTKETTY — agentin
    // rekisteröinti (build_family → bridge) nosti `agents_online`-gaugea, ja
    // siltakerroksen `task.*`/`contract.*`/`llm.*`/… -tapahtumat kasvattavat
    // vastaavia sarjoja recorderin kautta. Rekisteri jaetaan GatewayState:lle
    // Arc-jako-mallilla → /metrics näkee tarkalleen recorderin kasvattamat luvut.
    let state = Arc::new(GatewayState {
        bus: Some(runtime.bus().clone()),
        discord_channel: discord_ch,
        inject_token,
        discord_public_key,
        actions,
        turn_audit,
        scheduler,
        agency_config_path,
        metrics: Some(metrics),
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

/// Kestävyystilan tiivistelmä jonka `status`/`doctor` näyttää operaattorille.
///
/// Kentät kertovat **mitä [`build_family`] kytkee** nykyisellä
/// `FAMILYCLAW_DATA_DIR`-ympäristöllä, ilman salaisuuksia tai tiedostopolkuja:
/// onko prosessi kaatumiskestävässä (persistentissä) vai muistinvaraisessa
/// tilassa, sekä kytkettyjen [`ActionRuntime`]-pintojen lajitunnisteet.
struct DurabilityReport {
    /// `true` kun `FAMILYCLAW_DATA_DIR` on asetettu (persistentti, kaatumiskestävä).
    persistent: bool,
    /// Lähetys-outboxin lajitunniste ([`ActionRuntime::dispatch_outbox_kind`]),
    /// `"journal"` tai `"in-memory"`.
    dispatch_outbox_kind: &'static str,
    /// Odottavien hyväksyntöjen pinnan lajitunniste
    /// ([`ActionRuntime::pending_store_kind`]), `"journal"` tai `"in-memory"`.
    pending_store_kind: &'static str,
}

impl DurabilityReport {
    /// Muotoilee yksirivisen kestävyysyhteenvedon ilman tilaetuliitettä.
    ///
    /// Esim. `persistent (data_dir set); dispatch_outbox=journal;
    /// pending_store=journal` tai `in-memory (no FAMILYCLAW_DATA_DIR) —
    /// crash-survival OFF; dispatch_outbox=in-memory; pending_store=in-memory`.
    /// Tiedostopolkua **ei** paljasteta (vain `set`-läsnäolo).
    fn summary(&self) -> String {
        let mode = if self.persistent {
            "persistent (data_dir set)".to_string()
        } else {
            "in-memory (no FAMILYCLAW_DATA_DIR) — crash-survival OFF".to_string()
        };
        format!(
            "{mode}; dispatch_outbox={}; pending_store={}",
            self.dispatch_outbox_kind, self.pending_store_kind
        )
    }
}

/// Kokoaa [`DurabilityReport`]:n rakentamalla saman [`ActionRuntime`]:n kuin
/// [`build_family`] valitsisi nykyisellä `FAMILYCLAW_DATA_DIR`-ympäristöllä.
///
/// Ohut kuori [`durability_report_for`]:lle: lukee `FAMILYCLAW_DATA_DIR`:n
/// prosessin ympäristöstä (tyhjä = unset = muistinvarainen) ja delegoi.
///
/// # Errors
/// [`FamilyClawError::config`] jos persistentin polun journal-pintojen avaus
/// epäonnistuu (sama virhe jonka käynnistys antaisi).
async fn build_durability_report() -> Result<DurabilityReport> {
    let data_dir = std::env::var("FAMILYCLAW_DATA_DIR")
        .ok()
        .filter(|v| !v.is_empty());
    durability_report_for(data_dir.as_deref()).await
}

/// Kokoaa [`DurabilityReport`]:n annetulle data-hakemistolle (env-vapaa ydin).
///
/// `data_dir`:
/// - `Some(dir)` → persistentti polku: avaa samat journal-pinnat kuin
///   [`build_family`] (durable pending + task + dispatch outbox) ja lukee niiden
///   **todelliset** lajitunnisteet — ei kovakoodausta.
/// - `None` → muistinvarainen polku: kaikki pinnat oletuksissaan, ei levy-I/O:ta.
///
/// Lukemalla lajitunnisteet kytketyistä pinnoista
/// ([`ActionRuntime::dispatch_outbox_kind`] + [`ActionRuntime::pending_store_kind`])
/// raportti vastaa täsmälleen sitä kestävyyspolkua jonka palvelin saisi.
/// Persistentillä polulla journal-tiedostot avataan (idempotentti append-loki,
/// sama kuin käynnistyksessä). Haaroitus on env-vapaa → deterministisesti
/// testattavissa eksplisiittisellä hakemistolla.
///
/// # Errors
/// [`FamilyClawError::config`] jos persistentin polun journal-pintojen avaus
/// epäonnistuu (sama virhe jonka käynnistys antaisi).
async fn durability_report_for(data_dir: Option<&str>) -> Result<DurabilityReport> {
    // Sama haaroitus kuin build_familyssa: data-hakemisto ratkaisee persistentin
    // (journal) vs. muistinvaraisen (in-memory) polun.
    let runtime = if let Some(dir) = data_dir {
        let dir = std::path::PathBuf::from(dir);
        let pending_path = dir.join("pending_approvals.jsonl");
        let task_path = dir.join("action_tasks.jsonl");
        let dispatch_path = dir.join("dispatch_outbox.jsonl");
        // `with_durable_stores` avaa nyt itse kaatumiskestävän dispatch-outboxin
        // kolmannesta polusta — sama yhden kutsun kokoonpano kuin build_familyssa,
        // ei erillistä with_dispatch_outbox-ketjutusta eikä outboxin kaksoisavausta.
        ActionRuntime::with_durable_stores(pending_path, task_path, dispatch_path)
            .await
            .map_err(|e| {
                FamilyClawError::config(format!("durable action stores open failed: {e}"))
            })?
    } else {
        // Muistinvarainen polku: kaikki pinnat oletuksissaan, ei levyä.
        ActionRuntime::with_default_skills()
            .map_err(|e| FamilyClawError::config(format!("action runtime build failed: {e}")))?
    };

    Ok(DurabilityReport {
        persistent: data_dir.is_some(),
        dispatch_outbox_kind: runtime.dispatch_outbox_kind(),
        pending_store_kind: runtime.pending_store_kind(),
    })
}

/// Palauttaa hiekkalaatikon (sandbox) saatavuus-etiketin.
///
/// Delegoituu [`familyclaw_sandbox::sandbox_availability`]:lle, joka raportoi
/// **todellisen käännetyn backendin**: `wasmtime (host-import denial + fuel
/// cap)` kun `wasmtime`-passthrough-piirre on aktiivinen, muuten `none (noop)`.
/// Lukemalla saatavuuden suoraan sandbox-cratesta (eikä gatewayn omasta
/// irrallisesta lipusta) raportti ei voi valehdella: jos label sanoo
/// `wasmtime`, oikea backend on oikeasti käännetty mukaan. Deterministinen ja
/// salaisuudeton → sopii sekä `status`- että `doctor`-tulosteeseen.
fn sandbox_label() -> &'static str {
    familyclaw_sandbox::sandbox_availability()
}

/// Palauttaa aktiivisen muistin upotustarjoajan etiketin (Phase 3, D4).
///
/// Runtime kääräisee muistin `EmbeddingMemoryStore`:lla
/// [`DeterministicEmbedder`](familyclaw_embeddings::DeterministicEmbedder)-
/// oletustarjoajalla (riippuvuudeton, köyhyys-yhteensopiva). Raportoi tarjoajan
/// vakaan id:n + ulottuvuuden, jotta operaattori näkee mikä upotus on todella
/// käytössä. Deterministinen ja salaisuudeton → sopii `status`/`doctor`-
/// tulosteeseen. Kun feature-gated mallintarjoaja lisätään, tämä päivitetään
/// raportoimaan todellinen käännetty tarjoaja (kuten [`sandbox_label`]).
fn embedder_label() -> String {
    use familyclaw_embeddings::DeterministicEmbedder;
    format!(
        "{} (dim={})",
        DeterministicEmbedder::ID,
        DeterministicEmbedder::DEFAULT_DIMENSIONS
    )
}

/// Kysyy käynnissä olevan gatewayn tilan (`/healthz` + `/readyz`).
///
/// Lukee kuunteluosoitteen [`resolve_addr`]:n kautta ja tekee kaksi HTTP
/// GET -pyyntöä. Tulostaa kummankin endpointin tilan sekä **kestävyystilan**
/// ([`build_durability_report`]) ja **hiekkalaatikon saatavuuden**
/// ([`sandbox_label`]), jotta operaattori näkee mikä taustapinta on oikeasti
/// kytkettynä. Palaa `Ok(())` vain kun `/readyz` vastaa `200 OK`; muuten
/// [`FamilyClawError::bus`], jolloin prosessi päättyy nollasta poikkeavalla
/// exit-koodilla.
///
/// # Errors
/// - [`FamilyClawError::config`] jos kuunteluosoite on jäsentymätön.
/// - [`FamilyClawError::config`] jos persistentin polun journal-pintojen avaus
///   epäonnistuu kestävyysraporttia koottaessa.
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

    // Kestävä taustapinta + hiekkalaatikko: operaattori näkee mikä on
    // oikeasti kytkettynä (ei vain HTTP-elossaolo).
    let durability = build_durability_report().await?;
    println!("durability: {}", durability.summary());
    println!("sandbox: {}", sandbox_label());
    println!("embedder: {}", embedder_label());

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
// Peräkkäisiä tarkistuslohkoja (addr/port/env/durability/sandbox/…), kukin
// tulostaa oman rivinsä — pitkä mutta suoraviivainen diagnostiikkasekvenssi.
#[allow(clippy::too_many_lines)]
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
    // Kanavaton julkaisutila (`none`): ei vaadittuja kanava-envejä eikä reply-
    // kohdetta — gateway ajaa MockChannelillä (HTTP-pinta + /metrics toimivat).
    // Tämä on tuoreen `cargo install`in savutesti-tila: `serve` + `status`
    // ilman perhe-avaimia. Ohitetaan kanavakohtaiset env-tarkistukset kokonaan.
    let channel_keys: &[&str] = if channel_kind == "none" {
        &[]
    } else if channel_kind == "discord" {
        &[DISCORD_CHANNEL_ID_ENV, REPLY_TARGET_ENV]
    } else {
        &[
            TELEGRAM_TOKEN_ENV,
            TELEGRAM_CHANNEL_ID_ENV,
            REPLY_TARGET_ENV,
        ]
    };
    if channel_kind == "none" {
        println!("[OK]      channel   none (channel-less serve — MockChannel, no family keys)");
    } else {
        println!("[INFO]     channel   {channel_kind}");
    }
    for key in channel_keys {
        if std::env::var_os(key).is_some_and(|v| !v.is_empty()) {
            println!("[OK]      env       {key} set");
        } else {
            println!("[MISSING] env       {key}");
            ok = false;
        }
    }

    if channel_kind == "discord" {
        // Discord vaatii JOKO bot-tokenin (kaksisuuntainen) TAI webhookin (postaus).
        let has_bot = std::env::var_os(DISCORD_BOT_TOKEN_ENV).is_some_and(|v| !v.is_empty());
        let has_webhook = std::env::var_os(DISCORD_WEBHOOK_URL_ENV).is_some_and(|v| !v.is_empty());
        if has_bot {
            println!("[OK]      env       {DISCORD_BOT_TOKEN_ENV} set (kaksisuuntainen bot)");
        } else if has_webhook {
            println!("[OK]      env       {DISCORD_WEBHOOK_URL_ENV} set (webhook-postaus)");
        } else {
            println!("[MISSING] env       {DISCORD_BOT_TOKEN_ENV} tai {DISCORD_WEBHOOK_URL_ENV}");
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

    // Kestävä taustapinta: raportoi todelliset lajitunnisteet jotka build_family
    // kytkisi, ja varoita REHELLISESTI jos prosessi olisi muistinvaraisessa
    // tilassa — at-most-once-takuu kaatumisen yli vaatii journal-taustapinnan.
    // Varoitus ≠ virhe (ei kaada doctoria), mutta operaattorin pitää tietää.
    let durability = build_durability_report().await?;
    println!("[INFO]     durability {}", durability.summary());
    if !durability.persistent {
        println!(
            "[WARN]    durability in-memory mode — at-most-once-under-crash guarantee needs the \
             journal backend; in-memory does NOT survive a process crash (set FAMILYCLAW_DATA_DIR)"
        );
    }
    println!("[INFO]     sandbox   {}", sandbox_label());
    println!("[INFO]     embedder  {}", embedder_label());

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
            scheduler: None,
            agency_config_path: None,
            metrics: None,
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
            scheduler: None,
            agency_config_path: None,
            metrics: None,
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
            scheduler: None,
            agency_config_path: None,
            metrics: None,
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
    fn plan_load_env_fallback_and_json_parsing() {
        // YHDISTETTY testi: [`PLAN_ENV`] on PROSESSIN-LAAJUINEN ympäristömuuttuja,
        // joten kaksi erillistä testifunktiota (toinen `remove_var`, toinen
        // `set_var`) kilpailisivat rinnakkain ajettuna ja vuorottelisivat
        // toistensa tilan päälle. Tehdään molemmat tarkistukset PERÄKKÄIN saman
        // funktion sisällä — silloin env-muuttujaa ei jaeta säikeiden yli eikä
        // tulos riipu ajojärjestyksestä.

        // (a) Ilman PLAN_ENV:iä → sisäänrakennettu yhden solmun savutesti.
        std::env::remove_var(PLAN_ENV);
        let plan = load_orchestration_plan();
        assert_eq!(plan.id, "smoke");
        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(plan.nodes[0].id.as_str(), "n1");

        // (b) PLAN_ENV asetettuna → JSON jäsentyy solmuiksi.
        let json = r#"{"id":"p","nodes":[
            {"id":"a","title":"A","description":"da"},
            {"id":"b","title":"B","description":"db"}
        ]}"#;
        std::env::set_var(PLAN_ENV, json);
        let plan = load_orchestration_plan();
        assert_eq!(plan.id, "p");
        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(plan.nodes[1].id.as_str(), "b");
        assert_eq!(plan.nodes[1].title, "B");

        // (c) Siivous: palauta prosessin tila ennalleen, jotta mahdolliset muut
        //     samaa muuttujaa lukevat testit eivät näe roskaa.
        std::env::remove_var(PLAN_ENV);
        let plan = load_orchestration_plan();
        assert_eq!(plan.id, "smoke");
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
            scheduler: None,
            agency_config_path: None,
            metrics: None,
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
            scheduler: None,
            agency_config_path: None,
            metrics: None,
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
            scheduler: None,
            agency_config_path: None,
            metrics: None,
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
            scheduler: None,
            agency_config_path: None,
            metrics: None,
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
            scheduler: None,
            agency_config_path: None,
            metrics: None,
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
        assert!(item
            .get("redacted_summary")
            .and_then(|v| v.as_str())
            .is_some());
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
            scheduler: None,
            agency_config_path: None,
            metrics: None,
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
        let (status, _) = approve_pending(State(state), HeaderMap::new(), Path(unknown)).await;
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

        let (status, _) = approve_pending(State(state), HeaderMap::new(), Path(id)).await;
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
            scheduler: None,
            agency_config_path: None,
            metrics: None,
        });
        let (status, _) = approve_pending(
            State(state),
            HeaderMap::new(),
            Path(ApprovalId::new().to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── Phase 4: kill-switch -reitti (POST /tasks/{id}/enabled) ──────────────

    fn state_without_scheduler() -> Arc<GatewayState> {
        Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
        })
    }

    #[tokio::test]
    async fn killswitch_503_without_scheduler() {
        let state = state_without_scheduler();
        let (status, _) = set_task_enabled_route(
            State(state),
            HeaderMap::new(),
            Path(uuid::Uuid::from_u128(1).to_string()),
            Json(serde_json::json!({ "enabled": false })),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn killswitch_400_on_bad_id_and_missing_body() {
        use familyclaw_scheduler::Scheduler;
        let sched = Arc::new(tokio::sync::Mutex::new(Scheduler::new()));
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: Some(sched),
            agency_config_path: None,
            metrics: None,
        });
        // Epäkelpo UUID → 400.
        let (status, _) = set_task_enabled_route(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Path("not-a-uuid".to_string()),
            Json(serde_json::json!({ "enabled": true })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Puuttuva `enabled` → 400.
        let (status, _) = set_task_enabled_route(
            State(state),
            HeaderMap::new(),
            Path(uuid::Uuid::from_u128(1).to_string()),
            Json(serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn killswitch_toggles_known_task_and_404s_unknown() {
        use familyclaw_actions::SkillId;
        use familyclaw_scheduler::{ScheduledTask, ScheduledTaskId, Scheduler};
        let mut s = Scheduler::new();
        let task_uuid = uuid::Uuid::from_u128(42);
        s.register(ScheduledTask::with_id(
            ScheduledTaskId::from_uuid(task_uuid),
            SkillId::new(),
            serde_json::json!({}),
            chrono::Duration::seconds(60),
            "being",
        ));
        let sched = Arc::new(tokio::sync::Mutex::new(s));
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: Some(Arc::clone(&sched)),
            agency_config_path: None,
            metrics: None,
        });

        // Tunnettu tehtävä → 200, tila päivittyy.
        let (status, _) = set_task_enabled_route(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Path(task_uuid.to_string()),
            Json(serde_json::json!({ "enabled": false })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            sched
                .lock()
                .await
                .task_enabled(ScheduledTaskId::from_uuid(task_uuid)),
            Some(false)
        );

        // Tuntematon tehtävä → 404.
        let (status, _) = set_task_enabled_route(
            State(state),
            HeaderMap::new(),
            Path(uuid::Uuid::from_u128(999).to_string()),
            Json(serde_json::json!({ "enabled": false })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn killswitch_persists_to_agency_config() {
        use familyclaw_actions::SkillId;
        use familyclaw_scheduler::{AgencyConfig, ScheduledTask, ScheduledTaskId, Scheduler};
        let mut s = Scheduler::new();
        let task_uuid = uuid::Uuid::from_u128(77);
        let id = ScheduledTaskId::from_uuid(task_uuid);
        s.register(ScheduledTask::with_id(
            id,
            SkillId::new(),
            serde_json::json!({}),
            chrono::Duration::seconds(60),
            "being",
        ));
        let sched = Arc::new(tokio::sync::Mutex::new(s));

        // Eristetty config-polku tälle testille.
        let dir = std::env::temp_dir().join("familyclaw-gw-agency-persist-test");
        let path = dir.join("agency.json");
        let _ = std::fs::remove_file(&path);

        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: Some(Arc::clone(&sched)),
            agency_config_path: Some(path.clone()),
            metrics: None,
        });

        // Disabloi reitin kautta → pitää persistoitua tiedostoon.
        let (status, _) = set_task_enabled_route(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Path(task_uuid.to_string()),
            Json(serde_json::json!({ "enabled": false })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        // Tiedostoon kirjoittui disabled-merkintä.
        let cfg = AgencyConfig::load(&path).expect("load persisted");
        assert!(cfg.is_disabled(id), "kill-switch persistoitui configiin");

        // Käyttöön otto reitin kautta → poistuu configista.
        let (status, _) = set_task_enabled_route(
            State(state),
            HeaderMap::new(),
            Path(task_uuid.to_string()),
            Json(serde_json::json!({ "enabled": true })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let cfg = AgencyConfig::load(&path).expect("load");
        assert!(!cfg.is_disabled(id), "käyttöön otto poisti configista");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn approve_route_without_bus_is_503_and_does_not_consume() {
        // **Option A:** ilman bussia gateway ei voi välittää jatkoa agentille
        // (yhtään agenttia ei kuuntele) → rehellinen 503, EI hiljaista
        // onnistumista. Esitarkistus on read-only → hyväksyntää EI kuluteta:
        // se on yhä odottavissa pyynnön jälkeen (voi yrittää uudelleen).
        let (state, actions) = state_with_actions(); // bus: None
        let id = submit_pending(&actions).await;

        let (status, Json(body)) = approve_pending(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Path(id.clone()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "ilman bussia operaattori-approve = 503 (Option A vaatii serve-tilan)"
        );
        assert!(
            body.get("error")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("serve mode")),
            "503-virheviesti mainitsee serve-tilan, oli: {body}"
        );

        // Hyväksyntää EI kulutettu: se näkyy yhä /approvals/pending-listalla.
        let (list_status, Json(list_body)) =
            list_pending_approvals(State(state), HeaderMap::new()).await;
        assert_eq!(list_status, StatusCode::OK);
        let arr = list_body.as_array().expect("array body");
        assert_eq!(
            arr.len(),
            1,
            "503-haaran jälkeen hyväksyntä on yhä odottavissa (ei kulutettu)"
        );
        assert_eq!(
            arr[0].get("approval_id").and_then(|v| v.as_str()),
            Some(id.as_str()),
            "sama odottava hyväksyntä yhä listalla"
        );
    }

    #[tokio::test]
    async fn approve_route_with_bus_publishes_and_does_not_consume() {
        // **Option A onnistunut polku:** bussin kanssa gateway VALIDOI +
        // JULKAISEE `ResumeApproval`-signaalin → 200 `status: "resuming"`. Gateway
        // EI kuluta hyväksyntää (sen kuluttaa agentti); ilman agenttia tässä
        // testissä hyväksyntä jää odottavaksi → todiste että gateway ei tee
        // sivuvaikutusta eikä kuluta lupaa.
        use familyclaw_bus::ResonanceBus;

        let rt = ActionRuntime::with_default_skills().expect("default skills");
        let actions = Arc::new(Mutex::new(rt));
        let bus = ResonanceBus::start(None).await.expect("bus");
        let state = Arc::new(GatewayState {
            bus: Some(bus.clone()),
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: Some(Arc::clone(&actions)),
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
        });
        let id = submit_pending(&actions).await;

        let (status, Json(body)) = approve_pending(
            State(Arc::clone(&state)),
            HeaderMap::new(),
            Path(id.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "bussin kanssa approve = 200");
        assert_eq!(
            body.get("status").and_then(|v| v.as_str()),
            Some("resuming"),
            "200-runko ilmoittaa asynkronisen jatkon (resuming), oli: {body}"
        );
        // EI lopputulosta gatewayssä (Option A): ei task_id/awaiting-kenttiä.
        assert!(
            body.get("task_id").is_none() && body.get("awaiting_further_approval").is_none(),
            "Option A: gateway ei palauta lopputulosta (asynkroninen), oli: {body}"
        );

        // Gateway EI kuluttanut hyväksyntää — ilman agenttia se on yhä odottavissa.
        let (list_status, Json(list_body)) =
            list_pending_approvals(State(state), HeaderMap::new()).await;
        assert_eq!(list_status, StatusCode::OK);
        let arr = list_body.as_array().expect("array body");
        assert_eq!(
            arr.len(),
            1,
            "gateway ei kuluta hyväksyntää (sen kuluttaa agentti) → yhä odottavissa"
        );

        bus.stop();
    }

    // ---- Prometheus-mittarit (GET /metrics) ----

    /// Apuri: poimii `Content-Type`-otsikon arvon handlerin palauttamasta
    /// otsikkotaulukosta merkkijonona (testin luettavuuden vuoksi).
    fn content_type_of(headers: &[(axum::http::header::HeaderName, &'static str)]) -> &'static str {
        headers
            .iter()
            .find(|(name, _)| name == axum::http::header::CONTENT_TYPE)
            .map_or("", |(_, v)| v)
    }

    #[tokio::test]
    async fn metrics_route_503_without_registry() {
        // Ilman kytkettyä rekisteriä → 503 (ei paniikkia). Sisältötyyppi
        // pysyy text/plain myös virhevastauksessa.
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
        });
        let (status, headers, body) = metrics_handler(State(state)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(content_type_of(&headers).starts_with("text/plain"));
        assert!(body.contains("not configured"));
    }

    #[tokio::test]
    async fn metrics_route_200_text_plain_prometheus_body() {
        // Kytketään laivueen oletusrekisteri ja kasvatetaan yksi laskuri, jotta
        // runko sisältää sekä TYPE-rivin että ei-nollaisen arvon. Vienti on
        // deterministinen (nimijärjestys), joten testi ei voi olla epävakaa.
        let registry = MetricsRegistry::with_fleet_defaults();
        registry
            .counter(familyclaw_observability::COUNTER_TASKS_CREATED)
            .inc();
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: Some(registry),
        });

        let (status, headers, body) = metrics_handler(State(state)).await;

        // 200 + text/plain (Prometheus-eksposition sisältötyyppi).
        assert_eq!(status, StatusCode::OK);
        assert!(
            content_type_of(&headers).starts_with("text/plain"),
            "Prometheus-vienti on text/plain, oli: {}",
            content_type_of(&headers)
        );

        // Runko jäsentyy Prometheus-ekspositioksi: vähintään yksi TYPE-rivi ja
        // laivueen oletuksista tunnettu mittaririvi.
        assert!(body.contains("# TYPE tasks_created counter"));
        assert!(body.contains("tasks_created 1"));
        assert!(body.contains("# TYPE agents_online gauge"));
        assert!(body.contains("agents_online 0"));
        // Determinismi: vienti on nimijärjestyksessä → agents_online ennen
        // tasks_created (aakkosjärjestys), joten tulosteen järjestys on vakaa.
        let agents_at = body.find("agents_online").expect("agents_online present");
        let tasks_at = body.find("tasks_created").expect("tasks_created present");
        assert!(
            agents_at < tasks_at,
            "vienti on deterministisesti nimijärjestyksessä"
        );
    }

    /// **Aito HTTP-integraatiotesti:** sitoo [`build_router`]:lla kootun
    /// reitityksen väliaikaiseen loopback-porttiin (sama malli kuin [`serve`]),
    /// tarjoilee sen taustatehtävässä ja hakee `GET /metrics`:n oikealla
    /// HTTP-asiakkaalla ([`reqwest`], jo riippuvuutena). Tämä testaa koko ketjun:
    /// Router → reitti → handler → `Content-Type`-otsikko → Prometheus-runko
    /// aidon socketin yli, ei vain handler-funktiota suoraan.
    #[tokio::test]
    async fn metrics_route_http_integration_returns_prometheus_text() {
        let registry = MetricsRegistry::with_fleet_defaults();
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: Some(registry),
        });
        let app = build_router(state);

        // Sido portti 0 → käyttöjärjestelmä antaa vapaan portin (rinnakkais-
        // turvallinen, ei kovakoodattua porttia).
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");

        // Tarjoile reititin taustalla; abortoi testin lopuksi.
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/metrics"))
            .send()
            .await
            .expect("GET /metrics");

        // 200 OK.
        assert_eq!(resp.status().as_u16(), 200);
        // text/plain (Prometheus-eksposition sisältötyyppi).
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.starts_with("text/plain"),
            "Content-Type pitää olla text/plain, oli: {content_type}"
        );

        // Runko jäsentyy Prometheus-ekspositioksi (TYPE-rivi + tunnettu mittari).
        let body = resp.text().await.expect("body");
        assert!(
            body.contains("# TYPE"),
            "runko sisältää Prometheus-TYPE-rivin"
        );
        assert!(
            body.contains("agents_online"),
            "laivueen oletusmittari näkyy viennissä"
        );

        server.abort();
    }

    /// **End-to-end-todiste:** elävä siltatapahtuma liikuttaa laskuria JAETUSSA
    /// rekisterissä, ja `GET /metrics` heijastaa sen (>0).
    ///
    /// Tämä sulkee katselmoinnin lipittämän aukon ("ei pää-päähän-testiä että
    /// elävä tehtävä liikuttaa laskuria"): sama kytkentä kuin [`serve`]:ssä —
    /// [`EventRecorder`] tilaa [`FamilyBridge`]:n ENNEN tapahtumaa ja kasvattaa
    /// `metrics.clone()`-rekisteriä, ja TÄSMÄLLEEN sama rekisteri annetaan
    /// [`GatewayState`]:lle. Julkaistaan tapahtuma (`create_task` +
    /// `Custom("task.completed")`), valutetaan recorder, ja todistetaan että
    /// (a) jaetun rekisterin laskuri kasvoi ja (b) `GET /metrics` -runko näyttää
    /// laskuririvin arvolla > 0.
    #[tokio::test]
    async fn live_bridge_event_moves_counter_on_shared_registry_and_metrics_reflects_it() {
        // 1. SAMA jako-malli kuin serve():ssä: yksi rekisteri, kloonataan
        //    recorderille; alkuperäinen menee GatewayState:lle.
        let metrics = MetricsRegistry::with_fleet_defaults();
        let bridge = FamilyBridge::new();
        // Tilaa ENNEN tapahtumaa (EventBus toimittaa vain tilauksen jälkeiset).
        let mut recorder = EventRecorder::new(&bridge, metrics.clone());

        // 2. Elävä siltatapahtuma: tehtävän luonti (→ tasks_created) ja
        //    valmistuminen (→ tasks_completed Custom-etiketillä).
        bridge
            .create_task("live-task", None)
            .await
            .expect("create_task");
        bridge.bus().publish(familyclaw_bridge::Event::new(
            familyclaw_bridge::EventKind::Custom("task.completed".into()),
            None,
        ));

        // 3. Valuta tapahtumat → jaettuun rekisteriin.
        let drained = recorder.drain_once().await;
        assert_eq!(drained, 2, "kaksi tapahtumaa käsiteltiin");

        // 4a. Jaetun rekisterin laskuri kasvoi (sama instanssi).
        assert_eq!(
            metrics
                .counter(familyclaw_observability::COUNTER_TASKS_CREATED)
                .get(),
            1
        );
        assert_eq!(
            metrics
                .counter(familyclaw_observability::COUNTER_TASKS_COMPLETED)
                .get(),
            1
        );

        // 4b. GET /metrics (sama rekisteri GatewayState:ssa) näyttää >0.
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: Some(metrics),
        });
        let (status, _headers, body) = metrics_handler(State(state)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body.contains("tasks_created 1"),
            "elävä tehtävä näkyy /metrics:ssä arvolla 1, runko:\n{body}"
        );
        assert!(
            body.contains("tasks_completed 1"),
            "valmistuminen näkyy /metrics:ssä arvolla 1, runko:\n{body}"
        );
    }

    /// Luo prosessikohtaisen uniikin väliaikaishakemiston journal-testeille.
    ///
    /// Ei riipu `tempfile`-kratesta (sitä ei ole dev-deppeissä): yhdistää
    /// prosessi-ID:n + nanosekuntileiman, jotta rinnakkaiset testit eivät
    /// törmää. Kutsuja vastaa siivouksesta.
    fn unique_data_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "familyclaw-durability-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("luo testihakemisto");
        dir
    }

    #[tokio::test]
    async fn durability_report_in_memory_reflects_default_kinds() {
        // Ilman data-hakemistoa: muistinvarainen tila, molemmat pinnat in-memory.
        let report = durability_report_for(None)
            .await
            .expect("in-memory report builds");
        assert!(!report.persistent, "ei data_diriä → ei persistentti");
        assert_eq!(report.dispatch_outbox_kind, "in-memory");
        assert_eq!(report.pending_store_kind, "in-memory");
    }

    #[tokio::test]
    async fn durability_report_persistent_reflects_journal_kinds() {
        // Data-hakemiston kanssa: persistentti tila, molemmat pinnat journal.
        let dir = unique_data_dir("persistent");
        let dir_str = dir.to_str().expect("polku on UTF-8");
        let report = durability_report_for(Some(dir_str))
            .await
            .expect("persistent report builds");
        assert!(report.persistent, "data_dir set → persistentti");
        assert_eq!(report.dispatch_outbox_kind, "journal");
        assert_eq!(report.pending_store_kind, "journal");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn durability_summary_in_memory_contains_crash_survival_off() {
        // status/doctor näyttävät tämän rivin — in-memory tilassa sen pitää
        // sisältää "crash-survival OFF" + molempien pintojen lajitunnisteet.
        let report = DurabilityReport {
            persistent: false,
            dispatch_outbox_kind: "in-memory",
            pending_store_kind: "in-memory",
        };
        let line = report.summary();
        assert!(
            line.contains("in-memory (no FAMILYCLAW_DATA_DIR)"),
            "in-memory-tila näkyy: {line}"
        );
        assert!(
            line.contains("crash-survival OFF"),
            "kaatumiskestävyyden puuttuminen näkyy: {line}"
        );
        assert!(
            line.contains("dispatch_outbox=in-memory") && line.contains("pending_store=in-memory"),
            "molemmat lajitunnisteet näkyvät: {line}"
        );
    }

    #[test]
    fn durability_summary_persistent_contains_journal_kinds() {
        // status/doctor-rivi persistentissä tilassa: ei OFF-varoitusta, journal-pinnat.
        let report = DurabilityReport {
            persistent: true,
            dispatch_outbox_kind: "journal",
            pending_store_kind: "journal",
        };
        let line = report.summary();
        assert!(line.contains("persistent (data_dir set)"), "tila: {line}");
        assert!(
            !line.contains("crash-survival OFF"),
            "persistentissä tilassa ei OFF-varoitusta: {line}"
        );
        assert!(
            line.contains("dispatch_outbox=journal") && line.contains("pending_store=journal"),
            "journal-lajitunnisteet näkyvät: {line}"
        );
    }

    /// Apuri: rakentaa doctorin näyttämät kestävyysrivit raportista — sama
    /// muotoilu kuin `doctor()`-funktiossa, jotta varoituslogiikka on testattava
    /// ilman täyttä `doctor()`-ajoa (joka lukee prosessin globaalia ympäristöä).
    fn doctor_durability_lines(report: &DurabilityReport) -> Vec<String> {
        let mut lines = vec![format!("[INFO]     durability {}", report.summary())];
        if !report.persistent {
            lines.push(
                "[WARN]    durability in-memory mode — at-most-once-under-crash guarantee needs the \
                 journal backend; in-memory does NOT survive a process crash (set FAMILYCLAW_DATA_DIR)"
                    .to_string(),
            );
        }
        lines
    }

    #[test]
    fn doctor_in_memory_emits_crash_survival_warning() {
        // doctor muistinvaraisessa tilassa: REHELLINEN varoitus (ei kaada doctoria).
        let report = DurabilityReport {
            persistent: false,
            dispatch_outbox_kind: "in-memory",
            pending_store_kind: "in-memory",
        };
        let lines = doctor_durability_lines(&report);
        let joined = lines.join("\n");
        assert!(
            joined.contains("[WARN]") && joined.contains("at-most-once-under-crash"),
            "doctor varoittaa kaatumiskestävyyden puuttumisesta: {joined}"
        );
        assert!(
            joined.contains("does NOT survive a process crash"),
            "varoitus on rehellinen kaatumisselviytymisestä: {joined}"
        );
    }

    #[test]
    fn doctor_persistent_emits_no_crash_survival_warning() {
        // doctor persistentissä tilassa: vain INFO-rivi, ei kaatumisvaroitusta.
        let report = DurabilityReport {
            persistent: true,
            dispatch_outbox_kind: "journal",
            pending_store_kind: "journal",
        };
        let lines = doctor_durability_lines(&report);
        let joined = lines.join("\n");
        assert!(
            joined.contains("[INFO]") && joined.contains("dispatch_outbox=journal"),
            "doctor näyttää journal-pinnat: {joined}"
        );
        assert!(
            !joined.contains("[WARN]"),
            "persistentissä tilassa ei kaatumisvaroitusta: {joined}"
        );
    }

    #[test]
    fn sandbox_label_matches_compiled_feature() {
        // sandbox-etiketti seuraa käännösaikaista wasmtime-piirrettä.
        let label = sandbox_label();
        if cfg!(feature = "wasmtime") {
            assert_eq!(label, "wasmtime (host-import denial + fuel cap)");
        } else {
            assert_eq!(label, "none (noop)");
        }
    }

    #[test]
    fn embedder_label_reports_active_provider() {
        // Phase 3: status/doctor näyttää aktiivisen upotustarjoajan id + dim.
        let label = embedder_label();
        assert!(
            label.contains("deterministic-hash-v1"),
            "tarjoajan id: {label}"
        );
        assert!(label.contains("dim=256"), "ulottuvuus: {label}");
    }

    // ---- E2E: suspend → approve → resume → reply (Phase 1 §6, RED-todiste) ----

    /// **Skriptattu fake-LLM** (raaka TCP, OpenAI-yhteensopiva endpoint): palauttaa
    /// annetut JSON-rungot järjestyksessä, yksi per pyyntö. Sama kuvio kuin
    /// `familyclaw-agent`:n tool-loop-testeissä — ei ulkoista mock-kirjastoa, ei
    /// verkkoa ulospäin. Palauttaa base-URL:n (`http://127.0.0.1:PORT/v1`).
    async fn spawn_scripted_llm_e2e(bodies: Vec<String>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind scripted llm");
        let addr = listener.local_addr().expect("scripted llm addr");
        tokio::spawn(async move {
            for body in bodies {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}/v1")
    }

    /// OpenAI-vastausrunko: **yksi työkalukutsu** chat-completions-johdon
    /// muodossa — `type:"function"` + sisäkkäinen `function`, jonka `arguments`
    /// on **JSON-merkkijono**, ja `content` on `null`. Peilaa oikeaa provideria.
    fn e2e_body_tool_call(id: &str, name: &str, arguments: &serde_json::Value) -> String {
        let arguments_str =
            serde_json::to_string(arguments).expect("arguments serialize to JSON string");
        serde_json::json!({
            "choices": [ {
                "message": {
                    "role": "assistant",
                    "content": serde_json::Value::Null,
                    "tool_calls": [ {
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": arguments_str }
                    } ]
                },
                "finish_reason": "tool_calls"
            } ]
        })
        .to_string()
    }

    /// OpenAI-vastausrunko: **pelkkä teksti** → tool-loop pysähtyy (lopullinen vastaus).
    fn e2e_body_text(text: &str) -> String {
        serde_json::json!({ "choices": [ { "message": { "content": text } } ] }).to_string()
    }

    /// Hyväksyntää vaativa **laskeva** testitaito: kasvattaa jaettua atomista
    /// laskuria joka suorituksella → sivuvaikutuksen suorituskertojen suora
    /// mittari. Nimi `approval_skill` (LLM-työkalukutsu viittaa nimeen).
    #[derive(Debug, Clone)]
    struct E2eCountingApprovalSkill {
        count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    /// Testitaidon kiinteä, deterministinen UUID (ei `uuid!`-makroa).
    const E2E_APPROVAL_SKILL_UUID: u128 = 0x7e57_0000_0000_4000_8000_0000_0000_0001;

    #[async_trait::async_trait]
    impl familyclaw_actions::ActionExecutor for E2eCountingApprovalSkill {
        async fn execute(
            &self,
            request: familyclaw_actions::ActionRequest,
        ) -> familyclaw_actions::Result<familyclaw_actions::ActionResult> {
            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(familyclaw_actions::ActionResult::success(
                "counting approval action executed",
                serde_json::json!({ "executed": true }),
                request.now,
            ))
        }
    }

    impl familyclaw_actions::Skill for E2eCountingApprovalSkill {
        fn manifest(&self) -> familyclaw_actions::manifest::SkillManifest {
            familyclaw_actions::manifest::SkillManifest {
                id: familyclaw_actions::SkillId::from_uuid(uuid::Uuid::from_u128(
                    E2E_APPROVAL_SKILL_UUID,
                )),
                name: "approval_skill".to_string(),
                version: "1.0.0".to_string(),
                description:
                    "Laskeva ulkoisesti kirjoittava toiminto (vaatii hyväksynnän, E2E-testi)."
                        .to_string(),
                permissions: vec![familyclaw_actions::policy::SkillPermission::WriteExternal],
                risk: familyclaw_actions::policy::ActionRisk::WriteExternal,
                approval_policy: familyclaw_actions::policy::ApprovalPolicy::RequireApproval,
                input_hint: None,
                output_hint: None,
                input_schema: familyclaw_actions::manifest::default_input_schema(),
            }
        }
    }

    /// **End-to-end RED-todiste (Phase 1 §6 manuaaliportin aukko):** todistaa että
    /// operaattorin `POST /approvals/{id}/approve` **ajaa toiminnon sivuvaikutuksen
    /// mutta EI aja agenttia lopulliseen vastaukseen** — vuoro ei jatku
    /// (`turn_resumed`/`turn_answered` puuttuvat) eikä reply tavoita kanavaa.
    ///
    /// Harness kokoaa **aidon agentin** in-crate (skriptattu LLM + jaettu
    /// `ActionRuntime` laskevalla hyväksyntätaidolla + jaettu `AuditCollector` +
    /// captattu reply-sink). Sama `Arc<Mutex<ActionRuntime>>` ja sama
    /// `Arc<AuditCollector>` annetaan sekä agentille (`with_actions` /
    /// `with_turn_audit`) että `GatewayState`:lle — operaattori ja agentti jakavat
    /// TÄSMÄLLEEN saman lukitun tilan, kuten tuotannon `build_family`-kytkennässä.
    ///
    /// Vuoro suspendataan kutsumalla `agent.think()` suoraan (deterministinen,
    /// sama kuvio kuin `resume_approved_completes_turn_side_effect_runs_once`);
    /// agentti spawnataan sen jälkeen actoriksi ja bus annetaan `GatewayState`:lle,
    /// jotta myöhempi korjaus (`BusMessage::ResumeApproval` → actor-handler →
    /// `resume_approved` → reply-sink) voi tehdä väitteestä (e) vihreän
    /// koskematta tähän harnessiin.
    ///
    /// Väitteet (a)-(d) menevät LÄPI; (e) EPÄONNISTUU koska reply ei koskaan saavu
    /// eikä `turn_resumed`/`turn_answered` synny — tämä on aukon todiste.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn e2e_suspend_approve_resume_reply() {
        use familyclaw_agent::{new_reply_channel, Agent, ErasedMemoryStore, ThinkOutcome};
        use familyclaw_bus::{BusMessage, ResonanceBus};
        use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
        use familyclaw_memory::LocalJsonStore;
        use std::sync::atomic::Ordering::SeqCst;

        // 1. Bus (sama instanssi GatewayState:lle — tuleva ResumeApproval-julkaisu).
        let bus = ResonanceBus::start(None).await.expect("bus");

        // 2. Skriptattu LLM: ensin hyväksyntää vaativa työkalukutsu (suspend),
        //    sitten (resumen aikana) lopullinen teksti. Toista runkoa EI lueta
        //    tässä RED-testissä, koska gateway ei aja resumea — se on tarkoitus.
        // Payload sisältää SENTINEL-merkkijonon (tekosalaisuus, EI oikea avain eikä
        //    perheen nimi) jonka redaktoidun tiivistelmän pitää KARSIA — todistaa (b):ssä
        //    että /approvals/pending ei vuoda raakaa payloadia/salaisuuksia.
        let api = spawn_scripted_llm_e2e(vec![
            e2e_body_tool_call(
                "call_approve",
                "approval_skill",
                &serde_json::json!({ "q": "ship", "secret": "sk-SENTINEL-DO-NOT-LEAK" }),
            ),
            e2e_body_text("hyväksytty toiminto valmis"),
        ])
        .await;

        // 3. Jaettu ActionRuntime laskevalla hyväksyntätaidolla.
        let side_effect_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut rt = ActionRuntime::new();
        rt.register_skill(E2eCountingApprovalSkill {
            count: std::sync::Arc::clone(&side_effect_count),
        })
        .expect("register approval_skill");
        let actions: Arc<Mutex<ActionRuntime>> = Arc::new(Mutex::new(rt));

        // 4. Jaettu turn-audit-keräin.
        let turn_audit: Arc<AuditCollector> = Arc::new(AuditCollector::new());

        // 5. Captattu reply-sink: tällä HAVAINNOIMME tavoittaako lopullinen vastaus
        //    kanavan. Tuotannossa runtime omistaa recv-pään ja pumppaa Channel::send.
        let (sink, mut reply_rx) = new_reply_channel();

        // 6. Aito agentti skriptatulla LLM:llä + jaetut kahvat (sama kytkentä kuin
        //    build_family). Reply-kohde on staattinen fallback.
        let config = AgentConfig::new("e2e_agent", ModelConfig::new("scripted/model"));
        let soul = Soul::from_essence("I am the E2E agent.".to_string());
        let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
        let durable =
            DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
                .expect("durable ctx");
        let llm_cfg = familyclaw_agent::llm::LlmConfig::new(&api, "test-key", "scripted-model")
            .with_request_timeout_ms(2_000)
            .with_connect_timeout_ms(2_000);
        let agent = Agent::new(
            config,
            soul,
            memory,
            durable,
            bus.clone(),
            Some(llm_cfg),
            None,
        )
        .with_actions(Arc::clone(&actions))
        .with_turn_audit(Arc::clone(&turn_audit))
        .with_reply_sink(sink)
        .with_reply_target("e2e-channel");

        // 7. Aja vuoro → tool-loop suspendoituu hyväksyntää vaativaan työkaluun.
        //    Tämä synnyttää AIDON turn_suspended-auditin + odottavan hyväksynnän
        //    JAETTUUN ActionRuntimeen + jatkettavan vuoron resumable-pinnalle.
        let out = agent
            .think(&BusMessage::text("ship it"))
            .await
            .expect("think suspends");
        let approval_id = match out {
            ThinkOutcome::Suspended { approval_id, .. } => approval_id,
            other => panic!("odotettiin Suspended, sai: {other:?}"),
        };
        // Sivuvaikutus EI ole vielä ajettu (hyväksyntää ei myönnetty).
        assert_eq!(
            side_effect_count.load(SeqCst),
            0,
            "approval-gated side effect must NOT run before approve"
        );

        // 8. Spawnaa agentti actoriksi (pidetään elossa) — tuleva ResumeApproval-
        //    bus-signaali tavoittaa juuri tämän postilaatikon. RED-testi ei sitä
        //    vielä lähetä; pidämme actorin elossa harnessin uskollisuuden vuoksi.
        let _actor = agent.spawn().await.expect("spawn agent actor");

        // 9. GatewayState jakaa SAMAN actions- + turn_audit-kahvan ja saman busin.
        let state = Arc::new(GatewayState {
            bus: Some(bus.clone()),
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: Some(Arc::clone(&actions)),
            turn_audit: Some(Arc::clone(&turn_audit)),
            scheduler: None,
            agency_config_path: None,
            metrics: None,
        });
        let app = build_router(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        // (a) Vuoro suspendattiin: ei vielä replyä, ja /turns/audit sisältää
        //     turn_suspended-tapahtuman.
        assert!(
            reply_rx.try_recv().is_err(),
            "(a) suspendin jälkeen ei saa olla replyä reply-sinkissä"
        );
        let audit_body: String = client
            .get(format!("{base}/turns/audit"))
            .send()
            .await
            .expect("GET /turns/audit (a)")
            .text()
            .await
            .expect("audit body (a)");
        assert!(
            audit_body.contains("turn_suspended"),
            "(a) audit-jälki sisältää turn_suspended:n, oli:\n{audit_body}"
        );

        // (b) /approvals/pending palauttaa approval_id:n + redaktoidun
        //     tiivistelmän, EIKÄ vuoda salaisuuksia / perheen nimiä / yksityisiä
        //     absoluuttisia polkuja (Layer-B-henki).
        let pending_resp = client
            .get(format!("{base}/approvals/pending"))
            .send()
            .await
            .expect("GET /approvals/pending (b)");
        assert_eq!(pending_resp.status().as_u16(), 200, "(b) pending = 200");
        let pending_body = pending_resp.text().await.expect("pending body (b)");
        assert!(
            pending_body.contains(&approval_id.to_string()),
            "(b) pending sisältää approval_id:n, oli:\n{pending_body}"
        );
        // POSITIIVINEN redaktio-väite: tiivistelmä on TÄSMÄLLEEN
        // `ActionRuntime::pending_summary`:n redaktoima muoto (vain taidon nimi),
        // EI raaka payload. Tämä on sekä lähdekoodissa vuotamaton (ei perheen
        // nimiä literaaleina) että merkityksellisempi kuin pelkkä negatiivinen
        // tarkistus: se sitoo testin redaktoituun esitykseen.
        assert!(
            pending_body.contains("taito 'approval_skill' odottaa ihmisen hyväksyntää"),
            "(b) pending sisältää redaktoidun tiivistelmän (vain taidon nimi), oli:\n{pending_body}"
        );
        // Negatiiviset vuototarkistukset: ei avain-muotoista salaisuutta
        // (sk-/Bearer/test-key), ei SENTINEL-tekosalaisuutta, ei raakaa payloadia
        // (arvo `ship`, avain `"q"`/`"secret"`), ei yksityistä absoluuttista polkua.
        // SENTINEL todistaa AKTIIVISESTI että redaktio karsii payloadiin upotetut
        // salaisuudet — ei kosmeettinen tarkistus.
        let lowered = pending_body.to_lowercase();
        assert!(
            !lowered.contains("sk-")
                && !lowered.contains("bearer ")
                && !lowered.contains("test-key"),
            "(b) ei avain-muotoista salaisuutta: {pending_body}"
        );
        assert!(
            !pending_body.contains("SENTINEL"),
            "(b) redaktion pitää karsia payloadiin upotettu SENTINEL-tekosalaisuus: {pending_body}"
        );
        assert!(
            !pending_body.contains("ship")
                && !pending_body.contains("\"q\"")
                && !pending_body.contains("\"secret\""),
            "(b) ei raakaa payloadia: {pending_body}"
        );
        assert!(
            !pending_body.contains("C:\\") && !pending_body.contains("/home/"),
            "(b) ei yksityistä absoluuttista polkua: {pending_body}"
        );

        // (c) POST /approvals/{id}/approve → 200. **Option A:** 200 tarkoittaa
        //     "hyväksyntä otettu vastaan ja välitetty agentille" — EI että jatko on
        //     jo valmis. Sivuvaikutus + vastaus ajetaan asynkronisesti agentin
        //     resume-polulla (ResumeApproval-bus-signaali → handle_resume_signal).
        let approve_resp = client
            .post(format!("{base}/approvals/{approval_id}/approve"))
            .send()
            .await
            .expect("POST approve (c)");
        assert_eq!(approve_resp.status().as_u16(), 200, "(c) approve = 200");
        let approve_body: serde_json::Value =
            approve_resp.json().await.expect("approve body (c) json");
        assert_eq!(
            approve_body.get("status").and_then(|v| v.as_str()),
            Some("resuming"),
            "(c) 200-runko ilmoittaa asynkronisen jatkon (resuming), oli:\n{approve_body}"
        );

        // (d)+(e) **Asynkroninen, rajattu poll:** koska side-effect + vastaus
        //     ajetaan nyt agentissa 200:n palautumisen JÄLKEEN (Option A), emme voi
        //     väittää synkronisesti. Pollataan enintään ~3 s (60 × 50 ms) kunnes
        //     KAIKKI toteutuvat: lopullinen reply saapuu reply-sinkkiin, side-effect-
        //     laskuri == 1, ja /turns/audit sisältää turn_resumed:n + turn_answered:n.
        //     Rajattu (ei loputon) → testi pysyy deterministisenä ja nopeana.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut reply_body: Option<String> = None;
        let mut final_audit;
        loop {
            // Ime reply-sinkki ei-blokkaavasti (agentti työntää tänne kun resume
            // valmistuu). Säilytä ensimmäinen saapunut vastaus.
            if reply_body.is_none() {
                if let Ok(msg) = reply_rx.try_recv() {
                    reply_body = Some(msg.body);
                }
            }
            final_audit = client
                .get(format!("{base}/turns/audit"))
                .send()
                .await
                .expect("GET /turns/audit (e)")
                .text()
                .await
                .expect("audit body (e)");

            let done = reply_body.is_some()
                && side_effect_count.load(SeqCst) == 1
                && final_audit.contains("turn_resumed")
                && final_audit.contains("turn_answered");
            if done || std::time::Instant::now() >= deadline {
                break; // valmis tai timeout → väitteet alla raportoivat havainnon
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // Nämä väitteet ovat se ENTINEN RED-rivi: ne ovat nyt VIHREÄT, koska
        // operaattori-approve julkaisee ResumeApprovalin ja agentti jatkaa vuoron
        // loppuun (resume_approved → reply-sink). Älä löysennä niitä.
        assert_eq!(
            reply_body.as_deref(),
            Some("hyväksytty toiminto valmis"),
            "(e) lopullisen vastauksen pitää tavoittaa reply-sink approven jälkeen \
             (sai: {reply_body:?}); side_effect={}, audit:\n{final_audit}",
            side_effect_count.load(SeqCst)
        );
        // (d) Sivuvaikutus ajettiin TASAN KERRAN (eventually-exactly-once).
        assert_eq!(
            side_effect_count.load(SeqCst),
            1,
            "(d) approval-gated side effect must run exactly once (async, polled)"
        );
        assert!(
            final_audit.contains("turn_resumed"),
            "(e) audit-jäljen pitää sisältää turn_resumed approven jälkeen, oli:\n{final_audit}"
        );
        assert!(
            final_audit.contains("turn_answered"),
            "(e) audit-jäljen pitää sisältää turn_answered approven jälkeen, oli:\n{final_audit}"
        );

        // **Ei kaksoislaukaisua:** odota muutama lisäsykli ja varmista että
        // side-effect pysyy 1:ssä eikä toista vastausta saavu (hyväksyntä on
        // kertakäyttöinen → agentti ei voi ajaa sitä kahdesti).
        for _ in 0..5 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert_eq!(
                side_effect_count.load(SeqCst),
                1,
                "side-effect ei saa laueta toista kertaa (kertakäyttöinen hyväksyntä)"
            );
            assert!(
                reply_rx.try_recv().is_err(),
                "toista vastausta ei saa saapua (ei kaksoislaukaisua)"
            );
        }

        server.abort();
        bus.stop();
    }

    /// SF1 (GPT-5.5 review): **kaksi YHTÄAIKAISTA** `POST /approvals/{id}/approve`
    /// -pyyntöä samalle hyväksynnälle saa laukaista sivuvaikutuksen **korkeintaan
    /// kerran**. Aiempi `e2e_suspend_approve_resume_reply` todisti sekventiaalisen
    /// ei-kaksoislaukaisun; tämä todistaa että kilpa kahden samanaikaisen HTTP-
    /// pyynnön välillä ei riko kertakäyttöistä hyväksyntää.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn concurrent_double_approve_fires_side_effect_at_most_once() {
        use familyclaw_agent::{new_reply_channel, Agent, ErasedMemoryStore, ThinkOutcome};
        use familyclaw_bus::{BusMessage, ResonanceBus};
        use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
        use familyclaw_memory::LocalJsonStore;
        use std::sync::atomic::Ordering::SeqCst;

        let bus = ResonanceBus::start(None).await.expect("bus");
        let api = spawn_scripted_llm_e2e(vec![
            e2e_body_tool_call(
                "call_approve",
                "approval_skill",
                &serde_json::json!({ "q": "ship" }),
            ),
            e2e_body_text("hyväksytty toiminto valmis"),
        ])
        .await;

        let side_effect_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut rt = ActionRuntime::new();
        rt.register_skill(E2eCountingApprovalSkill {
            count: std::sync::Arc::clone(&side_effect_count),
        })
        .expect("register approval_skill");
        let actions: Arc<Mutex<ActionRuntime>> = Arc::new(Mutex::new(rt));
        let turn_audit: Arc<AuditCollector> = Arc::new(AuditCollector::new());
        let (sink, mut reply_rx) = new_reply_channel();

        let config = AgentConfig::new("e2e_agent", ModelConfig::new("scripted/model"));
        let soul = Soul::from_essence("I am the E2E agent.".to_string());
        let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
        let durable =
            DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
                .expect("durable ctx");
        let llm_cfg = familyclaw_agent::llm::LlmConfig::new(&api, "test-key", "scripted-model")
            .with_request_timeout_ms(2_000)
            .with_connect_timeout_ms(2_000);
        let agent = Agent::new(
            config,
            soul,
            memory,
            durable,
            bus.clone(),
            Some(llm_cfg),
            None,
        )
        .with_actions(Arc::clone(&actions))
        .with_turn_audit(Arc::clone(&turn_audit))
        .with_reply_sink(sink)
        .with_reply_target("e2e-channel");

        let out = agent
            .think(&BusMessage::text("ship it"))
            .await
            .expect("think suspends");
        let approval_id = match out {
            ThinkOutcome::Suspended { approval_id, .. } => approval_id,
            other => panic!("odotettiin Suspended, sai: {other:?}"),
        };
        assert_eq!(
            side_effect_count.load(SeqCst),
            0,
            "ei sivuvaikutusta ennen approvea"
        );

        let _actor = agent.spawn().await.expect("spawn agent actor");
        let state = Arc::new(GatewayState {
            bus: Some(bus.clone()),
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: Some(Arc::clone(&actions)),
            turn_audit: Some(Arc::clone(&turn_audit)),
            scheduler: None,
            agency_config_path: None,
            metrics: None,
        });
        let app = build_router(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/approvals/{approval_id}/approve");

        // Kaksi YHTÄAIKAISTA approve-pyyntöä samalle id:lle.
        let (r1, r2) = tokio::join!(client.post(&url).send(), client.post(&url).send(),);
        let s1 = r1.expect("POST approve #1").status().as_u16();
        let s2 = r2.expect("POST approve #2").status().as_u16();
        // Tasan yksi pyyntö saa kuluttaa kertakäyttöisen hyväksynnän (200); toinen
        // näkee sen jo kulutettuna (404 Not Found) tai myös 200 jos serialisointi
        // sallii — mutta sivuvaikutus alla on JOKA TAPAUKSESSA korkeintaan 1.
        let oks = u8::from(s1 == 200) + u8::from(s2 == 200);
        assert!(oks >= 1, "ainakin yksi approve onnistuu (sai {s1}/{s2})");

        // Odota että resume valmistuu, sitten varmista side-effect == 1 EIKÄ enää nouse.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if side_effect_count.load(SeqCst) >= 1 || std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        for _ in 0..6 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert!(
                side_effect_count.load(SeqCst) <= 1,
                "samanaikainen kaksois-approve EI saa laukaista sivuvaikutusta kahdesti (oli {})",
                side_effect_count.load(SeqCst)
            );
        }
        assert_eq!(
            side_effect_count.load(SeqCst),
            1,
            "sivuvaikutus ajetaan tasan kerran myös samanaikaisen kaksois-approven alla"
        );
        // Vain yksi lopullinen vastaus (ei kaksoislaukaisua reply-polulla).
        let mut replies = 0u8;
        while reply_rx.try_recv().is_ok() {
            replies += 1;
        }
        assert!(replies <= 1, "korkeintaan yksi vastaus (sai {replies})");

        server.abort();
        bus.stop();
    }

    /// SF2 (GPT-5.5 review): negatiivinen reitti-regressio joka VARTIOI axum 0.7
    /// -korjausta (`{approval_id}` → `:approval_id`). Jos joku palauttaisi reitin
    /// brace-syntaksiin, kirjaimellinen polkusegmentti tulkittaisiin literaaliksi
    /// eikä kaappaisi mielivaltaista id:tä → tämä testi punaistuisi.
    ///
    /// Todistus: POST mielivaltaiseen `:approval_id`-arvoon EI palauta 404
    /// "route not found" (reitti matchaa ja handler ajaa → 400/404/503 sen oman
    /// validoinnin mukaan), kun taas tuntematon polku palauttaa 404. Käytämme
    /// 503-erottelua: ilman actions-runtimea handler vastaa 503, joten matchannut
    /// reitti tuottaa 503 ja matchaamaton 404.
    #[tokio::test]
    async fn approve_route_captures_arbitrary_id_not_literal_braces() {
        // GatewayState ILMAN actions-runtimea → approve_pending vastaa 503 KUN
        // reitti matchaa. (Bearer-tarkistus ohitetaan kun inject_token = None.)
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
        });
        let app = build_router(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        // (1) Mielivaltainen id matchaa `:approval_id`-kaappauksen → handler ajaa →
        //     503 (ei actions-runtimea). Jos reitti olisi literaali `{approval_id}`,
        //     tämä EI matchaisi → 404. 503 todistaa että kaappaus toimii.
        let captured = client
            .post(format!("{base}/approvals/any-arbitrary-id-123/approve"))
            .send()
            .await
            .expect("POST arbitrary id");
        assert_eq!(
            captured.status().as_u16(),
            503,
            "mielivaltainen id matchaa reitin (handler ajaa, 503 ilman runtimea); \
             404 tarkoittaisi paluuta literaaliin {{approval_id}}-bugiin"
        );

        // (2) Kontrolli: täysin tuntematon polku palauttaa 404 (router toimii
        //     oikein, ei matchaa kaikkea).
        let unknown = client
            .post(format!("{base}/nonexistent/path"))
            .send()
            .await
            .expect("POST unknown path");
        assert_eq!(
            unknown.status().as_u16(),
            404,
            "tuntematon polku palauttaa 404 (router ei matchaa sokeasti kaikkea)"
        );

        server.abort();
    }

    /// **P0 approval-regressio (kilpa):** kaksi YHTÄAIKAISTA HTTP-tason
    /// `POST /approvals/{id}/approve` -pyyntöä samalle hyväksynnälle saavat laukaista
    /// ulkoisen sivuvaikutuksen **TASAN KERRAN**. Käyttää SAMAA aitoa E2E-harnessia
    /// kuin [`e2e_suspend_approve_resume_reply`] (aito axum-reititin + soketti +
    /// jaettu `ActionRuntime` laskevalla hyväksyntätaidolla + captattu reply-sink +
    /// jaettu `AuditCollector`).
    ///
    /// **Dokumentoitu semantiikka (Option A, sama kuin tuotanto):** hyväksyntä on
    /// kertakäyttöinen; ensimmäinen pyyntö kuluttaa sen ja palauttaa `200 resuming`
    /// (sivuvaikutus + vastaus ajetaan asynkronisesti agentin resume-polulla). Toinen
    /// rinnakkainen pyyntö joko (a) näkee hyväksynnän jo kulutettuna ja palauttaa
    /// turvallisen ei-onnistumisen (404), TAI (b) palauttaa myös 200 jos se ehtii
    /// ennen kulutusta — mutta kummassakin tapauksessa ulkoinen sivuvaikutus
    /// dispatchataan KORKEINTAAN KERRAN (kertakäyttöinen hyväksyntä serialisoituu
    /// jaetun `Mutex<ActionRuntime>`-lukon takana). Testi vahvistaa: tasan yksi 200
    /// EI ole pakollinen (rinnakkaisuus voi tuottaa 1 tai 2 × 200), mutta sivuvaikutus
    /// == 1, tasan yksi `turn_resumed`/`turn_answered`, korkeintaan yksi lopullinen reply,
    /// eikä actor kaadu/paniikkaa.
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn approval_double_post_race_runs_side_effect_once() {
        use familyclaw_agent::{new_reply_channel, Agent, ErasedMemoryStore, ThinkOutcome};
        use familyclaw_bus::{BusMessage, ResonanceBus};
        use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
        use familyclaw_memory::LocalJsonStore;
        use std::sync::atomic::Ordering::SeqCst;

        // 1. Bus + skriptattu LLM (suspend-työkalukutsu → lopullinen teksti) — sama
        //    kuvio kuin e2e_suspend_approve_resume_reply.
        let bus = ResonanceBus::start(None).await.expect("bus");
        let api = spawn_scripted_llm_e2e(vec![
            e2e_body_tool_call(
                "call_approve",
                "approval_skill",
                &serde_json::json!({ "q": "ship" }),
            ),
            e2e_body_text("hyväksytty toiminto valmis"),
        ])
        .await;

        // 2. Jaettu ActionRuntime laskevalla hyväksyntätaidolla (sivuvaikutusmittari).
        let side_effect_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut rt = ActionRuntime::new();
        rt.register_skill(E2eCountingApprovalSkill {
            count: std::sync::Arc::clone(&side_effect_count),
        })
        .expect("register approval_skill");
        let actions: Arc<Mutex<ActionRuntime>> = Arc::new(Mutex::new(rt));
        let turn_audit: Arc<AuditCollector> = Arc::new(AuditCollector::new());
        let (sink, mut reply_rx) = new_reply_channel();

        // 3. Aito agentti jaetuilla kahvoilla (sama kytkentä kuin build_family).
        let config = AgentConfig::new("e2e_agent", ModelConfig::new("scripted/model"));
        let soul = Soul::from_essence("I am the E2E agent.".to_string());
        let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
        let durable =
            DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
                .expect("durable ctx");
        let llm_cfg = familyclaw_agent::llm::LlmConfig::new(&api, "test-key", "scripted-model")
            .with_request_timeout_ms(2_000)
            .with_connect_timeout_ms(2_000);
        let agent = Agent::new(
            config,
            soul,
            memory,
            durable,
            bus.clone(),
            Some(llm_cfg),
            None,
        )
        .with_actions(Arc::clone(&actions))
        .with_turn_audit(Arc::clone(&turn_audit))
        .with_reply_sink(sink)
        .with_reply_target("e2e-channel");

        // 4. Aja vuoro → suspendoituu yhteen odottavaan hyväksyntään.
        let out = agent
            .think(&BusMessage::text("ship it"))
            .await
            .expect("think suspends");
        let approval_id = match out {
            ThinkOutcome::Suspended { approval_id, .. } => approval_id,
            other => panic!("odotettiin Suspended, sai: {other:?}"),
        };
        assert_eq!(
            side_effect_count.load(SeqCst),
            0,
            "sivuvaikutus EI saa ajaa ennen approvea"
        );

        // 5. Spawnaa agentti actoriksi (ResumeApproval-signaali tavoittaa sen
        //    postilaatikon) + GatewayState jakaa SAMAN actions/turn_audit/bus-kahvan.
        let _actor = agent.spawn().await.expect("spawn agent actor");
        let state = Arc::new(GatewayState {
            bus: Some(bus.clone()),
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: Some(Arc::clone(&actions)),
            turn_audit: Some(Arc::clone(&turn_audit)),
            scheduler: None,
            agency_config_path: None,
            metrics: None,
        });
        let app = build_router(state);
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/approvals/{approval_id}/approve");

        // 6. KAKSI YHTÄAIKAISTA approve-pyyntöä samalle id:lle (aito soketti).
        let (r1, r2) = tokio::join!(client.post(&url).send(), client.post(&url).send());
        let s1 = r1.expect("POST approve #1").status().as_u16();
        let s2 = r2.expect("POST approve #2").status().as_u16();
        // Semantiikka (dokumentoitu yllä): ainakin yksi 200 (resuming); toinen joko
        // 200 (ehti ennen kulutusta) tai 404 (jo kulutettu). Kumpikaan EI 5xx.
        let oks = u8::from(s1 == 200) + u8::from(s2 == 200);
        assert!(
            oks >= 1,
            "ainakin yksi rinnakkainen approve onnistuu (sai {s1}/{s2})"
        );
        assert!(
            s1 < 500 && s2 < 500,
            "kumpikaan rinnakkainen approve ei saa tuottaa 5xx-kaatumista (sai {s1}/{s2})"
        );
        for s in [s1, s2] {
            assert!(
                s == 200 || s == 404,
                "rinnakkainen approve on joko 200 (resuming) tai 404 (jo kulutettu), oli {s}"
            );
        }

        // 7. Odota että asynkroninen resume valmistuu, sitten todista invariantit.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut reply_count = 0u8;
        loop {
            while reply_rx.try_recv().is_ok() {
                reply_count += 1;
            }
            let audit = client
                .get(format!("http://{addr}/turns/audit"))
                .send()
                .await
                .expect("GET /turns/audit")
                .text()
                .await
                .expect("audit body");
            let done = side_effect_count.load(SeqCst) >= 1
                && audit.contains("turn_resumed")
                && audit.contains("turn_answered");
            if done || std::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // 8. **Kovat väitteet.** Sivuvaikutus ajettiin TASAN KERRAN — kilpa kahden
        //    HTTP-pyynnön välillä ei riko kertakäyttöistä hyväksyntää.
        assert_eq!(
            side_effect_count.load(SeqCst),
            1,
            "ulkoinen sivuvaikutus dispatchataan TASAN KERRAN myös rinnakkaisen \
             kaksois-approven alla (sai {})",
            side_effect_count.load(SeqCst)
        );
        // Auditin pitää näyttää TASAN yksi tehokas jatkettu vuoro (yksi turn_resumed
        // + yksi turn_answered) — ei kahta jatkoa.
        let final_audit = client
            .get(format!("http://{addr}/turns/audit"))
            .send()
            .await
            .expect("GET /turns/audit (final)")
            .text()
            .await
            .expect("audit body (final)");
        assert_eq!(
            final_audit.matches("turn_resumed").count(),
            1,
            "tasan yksi turn_resumed (hyväksyntä ei jatka vuoroa kahdesti), audit:\n{final_audit}"
        );
        assert_eq!(
            final_audit.matches("turn_answered").count(),
            1,
            "tasan yksi turn_answered (yksi lopullinen vastaus), audit:\n{final_audit}"
        );

        // 9. Imuroi reply-sinkki muutaman lisäsyklin ajan: odota että TASAN yksi
        //    lopullinen reply saapuu (auditissa on jo yksi turn_answered), ja
        //    varmista ettei sivuvaikutus enää nouse eikä toista vastausta saavu.
        //    `turn_answered == 1` (yllä) takaa että vastaus tuotettiin; tässä
        //    odotetaan että se myös tavoittaa reply-sinkin tasan kerran.
        for _ in 0..40 {
            while reply_rx.try_recv().is_ok() {
                reply_count += 1;
            }
            assert_eq!(
                side_effect_count.load(SeqCst),
                1,
                "sivuvaikutus ei saa laueta toista kertaa (kertakäyttöinen hyväksyntä)"
            );
            if reply_count >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        // Tyhjennä mahdolliset jälkijunan replyt ja vaadi TASAN yksi: ei nollaa
        // (vastaus on tuotettu) eikä kahta (ei kaksoislaukaisua).
        while reply_rx.try_recv().is_ok() {
            reply_count += 1;
        }
        assert_eq!(
            reply_count, 1,
            "tasan yksi lopullinen reply tavoittaa reply-sinkin (sai {reply_count})"
        );

        server.abort();
        bus.stop();
    }

    /// **P0 approval-regressio (reitti-syntaksi axum 0.7):** vartioi että hyväksyntä-
    /// reitti on rekisteröity `:approval_id`-kaappauksena EIKÄ kirjaimellisena
    /// brace-segmenttinä. axum 0.7 / matchit 0.7 tulkitsee brace-muotoisen segmentin
    /// literaaliksi polkusegmentiksi, joten brace-reitti EI matchaa todellisia id:itä.
    ///
    /// **Empiirisesti todettu semantiikka (tämä repo, axum 0.7.9 / matchit 0.7.3):**
    /// - Oikea reitti `:approval_id`: mielivaltainen id (ml. oikea UUID) MATCHAA →
    ///   handler ajaa → 503 (ilman actions-runtimea). Myös kirjaimellinen brace-
    ///   segmentti matchaa, koska se on vain yksi kaapattu arvo → 503.
    /// - BUGATTU brace-reitti (literaali): KAIKKI pyynnöt — sekä oikea UUID ETTÄ
    ///   kirjaimellinen brace-polku — palauttavat 404 (literaali ei matchaa oikeaa
    ///   id:tä; empiirisesti todennettu probella ennen tämän testin kirjoittamista).
    ///
    /// Ratkaiseva erotin regression havaitsemiseksi on siis **OIKEA UUID matchaa
    /// (503, ei 404)**. Jos joku palauttaa reitin brace-muotoon, oikea UUID alkaa
    /// palauttaa 404 → tämä testi punaistuu (todennettu temp-revertillä). Brace-polun
    /// käytös dokumentoidaan ja varmistetaan ettei se tuota onnistunutta hyväksyntää.
    #[tokio::test]
    async fn approval_literal_braces_route_does_not_match_on_axum_07() {
        // GatewayState ILMAN actions-runtimea → matchannut reitti vastaa 503,
        // matchaamaton reitti vastaa 404. (Bearer ohitetaan kun inject_token = None.)
        let state = Arc::new(GatewayState {
            bus: None,
            discord_channel: None,
            inject_token: None,
            discord_public_key: None,
            actions: None,
            turn_audit: None,
            scheduler: None,
            agency_config_path: None,
            metrics: None,
        });
        let app = build_router(state);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        // (1) RATKAISEVA: oikea UUID tavoittaa hyväksyntä-handlerin → 503 (ei runtimea).
        //     Jos reitti olisi literaali brace-muoto, tämä palauttaisi 404 ja testi
        //     punaistuisi. TÄMÄ rivi pakottaa `:approval_id`-syntaksin.
        let real_uuid = "11111111-1111-4111-8111-111111111111";
        let real = client
            .post(format!("{base}/approvals/{real_uuid}/approve"))
            .send()
            .await
            .expect("POST real uuid");
        assert_eq!(
            real.status().as_u16(),
            503,
            "oikea UUID tavoittaa approval-handlerin (503 ilman runtimea); 404 \
             tarkoittaisi paluuta literaaliin brace-reittiin (axum 0.7 -bugi)"
        );

        // (2) Kirjaimellinen brace-polku `/approvals/{{approval_id}}/approve` EI saa
        //     tuottaa ONNISTUNUTTA hyväksyntää. Oikean `:approval_id`-reitin alla se
        //     matchaa kaapattuna arvona ja päätyy 503:een (ei runtimea) — EI 2xx.
        //     Tämä todistaa ettei kirjaimellinen brace ole erikoiskäsitelty
        //     onnistumispolku.
        let braces = client
            .post(format!("{base}/approvals/{{approval_id}}/approve"))
            .send()
            .await
            .expect("POST literal braces");
        let braces_status = braces.status().as_u16();
        assert!(
            !(200..300).contains(&braces_status),
            "kirjaimellinen brace-polku ei saa tuottaa onnistunutta hyväksyntää (oli {braces_status})"
        );

        // (3) Kontrolli: täysin tuntematon polku palauttaa 404 (router ei matchaa
        //     sokeasti kaikkea) — varmistaa että 503 yllä on aito reitti-match eikä
        //     catch-all.
        let unknown = client
            .post(format!("{base}/nonexistent/path"))
            .send()
            .await
            .expect("POST unknown path");
        assert_eq!(
            unknown.status().as_u16(),
            404,
            "tuntematon polku palauttaa 404 (router ei matchaa sokeasti kaikkea)"
        );

        server.abort();
    }
}
