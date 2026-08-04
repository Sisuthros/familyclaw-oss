//! Deep readiness check (`/readyz`) and canary (`POST /canary`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::http::StatusCode;
use axum::Json;
use familyclaw_actions::task::{DurableTaskQueue, TaskQueue, TaskStatus};
use familyclaw_agent::llm::{LlmMessage, ToolDefinition};
use familyclaw_agent::{build_llm_chain, EnvEndpointResolver};
use familyclaw_channels::DiscordChannel;
use familyclaw_core::{FamilyClawError, ModelConfig, Result};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tracing::warn;

/// Shared readiness-probe state for the gateway.
#[derive(Clone, Default)]
pub struct ReadinessProbe {
    /// LLM chain (primary + fallbacks) — same as the agent runtime.
    pub model: Option<ModelConfig>,
    /// Discord channel in webhook/bot mode.
    pub discord: Option<Arc<DiscordChannel>>,
    /// Writable directory for the durable journal.
    pub data_dir: Option<PathBuf>,
    /// Set when the LLM checks are **intentionally** skipped: keyless demo
    /// mode (`FAMILYCLAW_CHANNEL_KIND=none` + no `FAMILYCLAW_PROVIDERS`).
    /// The reason is reported verbatim in `/readyz` under `degraded`, so a
    /// skipped check is visible rather than silently dropped.
    ///
    /// `None` → the LLM checks run and gate readiness (fail-closed default
    /// for every real deployment).
    pub llm_skipped: Option<String>,
}

/// Result of a single check.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// JSON response for the deep `/readyz`.
#[derive(Debug, Clone, Serialize)]
pub struct ReadyzResponse {
    pub ready: bool,
    /// Checks that were intentionally skipped, with the reason. Empty (and
    /// omitted from the JSON) in every normal deployment — a non-empty list
    /// means the gateway is up but knowingly running with less than full
    /// capability (e.g. keyless demo mode without an LLM provider).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded: Vec<String>,
    pub checks: Vec<CheckResult>,
}

/// JSON response for the canary.
#[derive(Debug, Clone, Serialize)]
pub struct CanaryResponse {
    pub ok: bool,
    pub latency_ms: u64,
    pub checks: Vec<CheckResult>,
}

/// Checks whether the journal directory is writable.
pub async fn check_journal_writable(data_dir: &std::path::Path) -> CheckResult {
    let probe = data_dir.join(".readyz_probe");
    match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&probe)
        .await
    {
        Ok(mut f) => {
            let line = format!("probe {}\n", familyclaw_core::time::now().timestamp());
            match f.write_all(line.as_bytes()).await {
                Ok(()) => CheckResult {
                    name: "journal_writable",
                    ok: true,
                    detail: "append ok".into(),
                },
                Err(e) => CheckResult {
                    name: "journal_writable",
                    ok: false,
                    detail: format!("write failed: {e}"),
                },
            }
        }
        Err(e) => CheckResult {
            name: "journal_writable",
            ok: false,
            detail: format!("open failed: {e}"),
        },
    }
}

/// Overall deadline for the entire `llm_ping` probe. The per-attempt timeout is 8s
/// (`probe_resolver_from_env`), but without an OVERALL deadline, failover can walk
/// 4 models x 2 passes = worst case 8x8s = 64s (audit finding [1]: measured 3.5-19.7s).
/// 20s cuts off a failover stall WHILE still letting a single slow LLM ping through
/// (canary measurement 3.9-8s on the NIM route). watchdog.ps1's readyz timeout is 60s,
/// so 20s stays safely under it without cutting off valid pings (10s was too
/// tight: ~40% of pings hit it, measured on a production agent on 2026-07-09).
const LLM_PING_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);
const LLM_TOOLS_PING_DEADLINE: std::time::Duration = std::time::Duration::from_secs(45);

/// Max output tokens for the plain [`check_llm_ping`] probe. A short "pong"
/// needs only a couple of tokens (Fable 5 diagnosis 2026-07-09, see
/// [`probe_resolver_from_env`]).
///
/// Fix 2026-07-31: 32 → 512. Yhden käyttöönoton primary vaihdettu
/// `ollama/kimi-k2.7-code:cloud` (reasoning-malli, Ollama Cloud Pro -avain).
/// Kimi tuottaa `reasoning`-kentän (~140-190 tok) ENNEN contentia — sama
/// ongelma kuin `llm_tools_ping` 2026-07-25. 32 tok katkaisi reasoningin →
/// `llm_ping deadline 20s exceeded` vaikka malli vastaa 2s:ssa suoraan.
/// 512 jättää 2.5x headwayn kuten tools-pingillä.
const LLM_PING_MAX_TOKENS: u32 = 512;

/// Max output tokens for [`check_llm_tools_ping`].
///
/// ROOT CAUSE (2026-07-25): this probe used to share [`LLM_PING_MAX_TOKENS`]
/// (32) with the plain ping via a single hardcoded resolver. Reasoning
/// models (NIM's `nvidia/llama-3.3-nemotron-super-49b-v1.5` and its
/// predecessor `nemotron-3-ultra-550b`) emit a `reasoning_content` trace
/// BEFORE the `tool_calls` payload -- measured 140-190 completion tokens for
/// this probe's single-tool "call `fs_read_allowlisted`" turn against NVIDIA
/// NIM directly. A 32-token cap truncates the response mid-reasoning: no
/// `tool_calls` ever appears (`/readyz` reported
/// `llm_tools_ping: "completion ok but no tool_calls"`), or the function-call
/// JSON itself gets cut mid-argument and NIM's vLLM backend 400s with
/// "Invalid JSON: EOF while parsing a value". A direct curl with a bigger
/// budget returns real `tool_calls` for the SAME model, which is what
/// pointed at the token budget rather than the model or the request shape
/// (`tool_choice: "required"` was already serialized correctly, and
/// `reasoning_content` is simply an extra field the response parser ignores
/// -- it does not interfere with `tool_calls` parsing once the response
/// isn't truncated). 512 leaves roughly 2.5x headway over the observed
/// worst case while staying well under [`LLM_TOOLS_PING_DEADLINE`]'s 45s
/// budget.
const LLM_TOOLS_PING_MAX_TOKENS: u32 = 512;

/// Lightweight LLM ping (one short completion) — uses the same provider resolver as serve.
pub async fn check_llm_ping(model_cfg: &ModelConfig) -> CheckResult {
    let resolver = probe_resolver_from_env(LLM_PING_MAX_TOKENS);
    match build_llm_chain(model_cfg, &resolver) {
        Ok(chain) => {
            let messages = [LlmMessage::user("ping")];
            // SAFETY FIX 2026-07-09 (audit [1]): an overall deadline around the
            // whole failover walk — otherwise readyz times out when several
            // models time out 8s in a row.
            match tokio::time::timeout(LLM_PING_DEADLINE, chain.complete(&messages)).await {
                Ok(Ok(_)) => CheckResult {
                    name: "llm_ping",
                    ok: true,
                    detail: "completion ok".into(),
                },
                Ok(Err(e)) => CheckResult {
                    name: "llm_ping",
                    ok: false,
                    detail: format!("completion failed: {e}"),
                },
                Err(_) => CheckResult {
                    name: "llm_ping",
                    ok: false,
                    detail: format!(
                        "llm_ping deadline {}s exceeded",
                        LLM_PING_DEADLINE.as_secs()
                    ),
                },
            }
        }
        Err(e) => CheckResult {
            name: "llm_ping",
            ok: false,
            detail: format!("resolver failed: {e}"),
        },
    }
}

/// LLM tool-calling probe — verifies that the primary returns `tool_calls`.
pub async fn check_llm_tools_ping(model_cfg: &ModelConfig) -> CheckResult {
    let resolver = probe_resolver_from_env(LLM_TOOLS_PING_MAX_TOKENS);
    match build_llm_chain(model_cfg, &resolver) {
        Ok(chain) => {
            let tools = [ToolDefinition {
                name: "fs_read_allowlisted".to_string(),
                description: "Read allowlisted file".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "read_full_content": { "type": "boolean" }
                    },
                    "required": ["path"]
                }),
            }];
            let messages = [LlmMessage::user(
                "Call fs_read_allowlisted on path memory.md with read_full_content false.",
            )];
            match tokio::time::timeout(
                LLM_TOOLS_PING_DEADLINE,
                chain.complete_with_tools_choice(&messages, &tools, Some("required")),
            )
            .await
            {
                Ok(Ok(result)) if result.has_tool_calls() => CheckResult {
                    name: "llm_tools_ping",
                    ok: true,
                    detail: "tool_calls ok".into(),
                },
                Ok(Ok(_)) => CheckResult {
                    name: "llm_tools_ping",
                    ok: false,
                    detail: "completion ok but no tool_calls".into(),
                },
                Ok(Err(e)) => CheckResult {
                    name: "llm_tools_ping",
                    ok: false,
                    detail: format!("tool completion failed: {e}"),
                },
                Err(_) => CheckResult {
                    name: "llm_tools_ping",
                    ok: false,
                    detail: format!(
                        "llm_tools_ping deadline {}s exceeded",
                        LLM_TOOLS_PING_DEADLINE.as_secs()
                    ),
                },
            }
        }
        Err(e) => CheckResult {
            name: "llm_tools_ping",
            ok: false,
            detail: format!("resolver failed: {e}"),
        },
    }
}

/// State of the Discord gateway connection.
pub async fn check_discord(dc: &DiscordChannel) -> CheckResult {
    let connected = dc.is_gateway_connected().await;
    CheckResult {
        name: "discord_gateway",
        ok: connected,
        detail: if connected {
            "connected".into()
        } else {
            "not connected".into()
        },
    }
}

/// Työtilajuurien tila ilman polkujen paljastamista: (konfiguroidut, olemassa).
///
/// Palauttaa vain lukumäärät — `/readyz` on julkinen pinta, eikä sinne saa
/// vuotaa koneen hakemistorakennetta.
fn workspace_scope_status(variable: &str) -> (usize, usize) {
    let roots: Vec<PathBuf> = std::env::var_os(variable)
        .map(|raw| {
            std::env::split_paths(&raw)
                .filter(|path| !path.as_os_str().is_empty())
                .collect()
        })
        .unwrap_or_default();
    // Fix 2026-07-31: `is_dir()` → `exists()`. Allowlist saa laillisesti
    // sisältää YKSITTÄISIÄ TIEDOSTOJA (esim.
    // `E:\workspace\data\agency.json` — schedule_task:n kohde). is_dir() laski
    // tiedoston "ei olemassa olevaksi" → readyz file_write_scope = 4/5 vaikka
    // kaikki polut ovat olemassa. exists() kattaa sekä kansiot että tiedostot.
    let existing = roots.iter().filter(|path| path.exists()).count();
    (roots.len(), existing)
}

/// Tulos työtilajuurien tarkistuksesta.
///
/// Kolme tilaa, ei kahta:
/// - **konfiguroitu ja ehjä** → `Ok`-tarkistus, `/readyz` pysyy vihreänä;
/// - **konfiguroitu mutta rikki** (juuri puuttuu levyltä) → **kaatuva
///   tarkistus**. Tämä on se hiljainen tuotantovika jota kukaan ei huomaa:
///   allowlist on olemassa, mutta osoittaa hakemistoon jota ei ole, joten
///   taito kieltää kaiken ilman että mikään hälyttää;
/// - **tyhjä** → ei tarkistusta vaan `degraded`-merkintä. Tyhjä allowlist ei
///   ole virhe: taito on silloin oikein lukittu (fail-closed), mutta sen
///   pitää näkyä eikä kadota hiljaisuuteen.
///
/// Ero PR #58:aan: siellä nämä ajettiin vain jos
/// `FAMILYCLAW_REQUIRE_WORKSPACE_TOOLS=1` oli asetettu — eli oletuksena ei
/// koskaan. Täällä ne ajetaan aina, ilman lippua.
fn workspace_scope_check(
    name: &'static str,
    variable: &str,
) -> std::result::Result<CheckResult, String> {
    let (configured, existing) = workspace_scope_status(variable);
    if configured == 0 {
        return Err(format!(
            "{name}: {variable} is empty; the skill is fail-closed (no allowed roots)"
        ));
    }
    Ok(CheckResult {
        name,
        ok: configured == existing,
        detail: if existing == configured {
            format!("{configured} scoped root(s) configured")
        } else {
            format!("{existing}/{configured} configured roots exist on disk")
        },
    })
}

/// Työtilajuuret jotka `/readyz` tarkistaa. `(tarkistuksen nimi, env-muuttuja)`.
const WORKSPACE_SCOPES: [(&str, &str); 2] = [
    ("fs_read_scope", "FAMILYCLAW_FS_READ_ALLOW"),
    ("file_write_scope", "FAMILYCLAW_FILE_WRITE_ALLOW"),
];

/// Deep readyz: bus + LLM + Discord + journal + työtilajuuret.
pub async fn deep_readyz(
    bus_ok: bool,
    probe: &ReadinessProbe,
) -> (StatusCode, Json<ReadyzResponse>) {
    let mut checks = Vec::new();
    let mut degraded = Vec::new();
    checks.push(CheckResult {
        name: "resonance_bus",
        ok: bus_ok,
        detail: if bus_ok { "running" } else { "not running" }.into(),
    });

    // LLM checks: run by default (fail-closed — a deployment that HAS a
    // provider table but cannot reach it is genuinely not ready). They are
    // skipped ONLY in keyless demo mode, where no provider was ever asked
    // for; skipping is then reported under `degraded` instead of being
    // reported as a failed check, because nothing actually failed.
    // `POST /canary` stays the strict "can this box do a real LLM turn?"
    // surface and still hard-fails without a model (see `run_canary`).
    match (probe.llm_skipped.as_ref(), probe.model.as_ref()) {
        (Some(reason), _) => degraded.push(reason.clone()),
        (None, Some(model)) => {
            checks.push(check_llm_ping(model).await);
            checks.push(check_llm_tools_ping(model).await);
        }
        (None, None) => {}
    }

    if let Some(dc) = &probe.discord {
        checks.push(check_discord(dc).await);
    }

    if let Some(dir) = &probe.data_dir {
        checks.push(check_journal_writable(dir).await);
    }

    // Työtilan kykyrajat: aina, ei lipun takana. Rikkinäinen allowlist on
    // kaatava tarkistus, tyhjä allowlist on näkyvä `degraded`-merkintä.
    for (name, variable) in WORKSPACE_SCOPES {
        match workspace_scope_check(name, variable) {
            Ok(check) => checks.push(check),
            Err(reason) => degraded.push(reason),
        }
    }

    let ready = checks.iter().all(|c| c.ok);
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(ReadyzResponse {
            ready,
            degraded,
            checks,
        }),
    )
}

/// Canary: synthetic LLM turn + infra checks.
pub async fn run_canary(probe: &ReadinessProbe) -> Result<Json<CanaryResponse>> {
    let started = Instant::now();
    let mut checks = Vec::new();

    if let Some(model) = probe.model.as_ref() {
        checks.push(check_llm_ping(model).await);
    } else {
        checks.push(CheckResult {
            name: "llm_ping",
            ok: false,
            detail: "no model configured".into(),
        });
    }

    if let Some(dc) = &probe.discord {
        checks.push(check_discord(dc).await);
    }

    if let Some(dir) = &probe.data_dir {
        checks.push(check_journal_writable(dir).await);
    }

    let ok = checks.iter().all(|c| c.ok);
    if !ok {
        warn!("canary check failed: {:?}", checks);
    }

    Ok(Json(CanaryResponse {
        ok,
        latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        checks,
    }))
}

/// Env var holding the probe's provider table (`prefix=base_url=KEY_ENV;…`).
const PROVIDERS_ENV: &str = "FAMILYCLAW_PROVIDERS";

/// Number of usable provider entries in [`PROVIDERS_ENV`].
///
/// This is the single source of truth for "is an LLM provider configured at
/// all?" — the probes can ONLY reach an endpoint through this table (see
/// [`probe_resolver_from_env`]), so `0` means no LLM was ever wired up, not
/// that one broke. Uses the same parse rules as the resolver so the two can
/// never disagree.
#[must_use]
pub fn configured_provider_count() -> usize {
    let Ok(spec) = std::env::var(PROVIDERS_ENV) else {
        return 0;
    };
    count_provider_entries(&spec)
}

/// Pure parse half of [`configured_provider_count`] (env-free → testable
/// without mutating process-global state).
fn count_provider_entries(spec: &str) -> usize {
    spec.split(';')
        .filter(|entry| !entry.trim().is_empty())
        .filter(|entry| {
            let parts: Vec<&str> = entry.splitn(3, '=').map(str::trim).collect();
            match parts.as_slice() {
                [prefix, base_url, key_field] => {
                    !prefix.is_empty()
                        && !base_url.is_empty()
                        && key_field.split(',').any(|k| !k.trim().is_empty())
                }
                _ => false,
            }
        })
        .count()
}

/// Builds the readiness probe at `serve()` startup.
///
/// `channel_kind` decides the readiness SEMANTIC when no provider table is
/// configured:
/// - `"none"` (keyless demo / guest path) → the LLM checks are skipped and
///   reported under `degraded`; `/readyz` reports platform readiness. The
///   operator explicitly asked for a keyless run, so a missing provider is
///   an intentional state, not a fault.
/// - anything else (telegram/discord/slack — a real deployment) → the LLM
///   checks run and fail closed, so a forgotten key still shows up as 503
///   and a load balancer will not route turns to a mute gateway.
pub fn build_probe(
    model: Option<ModelConfig>,
    discord: Option<Arc<DiscordChannel>>,
    channel_kind: &str,
) -> ReadinessProbe {
    let data_dir = std::env::var("FAMILYCLAW_DATA_DIR").ok().map(PathBuf::from);
    let llm_skipped = if channel_kind == "none" && configured_provider_count() == 0 {
        let reason = format!(
            "llm_ping/llm_tools_ping skipped: no LLM provider configured ({PROVIDERS_ENV} unset) \
             and channel kind is 'none' (keyless demo mode) — the agent runs MUTE (memory + \
             emotion only, no text replies). POST /canary asserts a live LLM turn."
        );
        warn!("{reason}");
        Some(reason)
    } else {
        None
    };
    ReadinessProbe {
        model,
        discord,
        data_dir,
        llm_skipped,
    }
}

/// Provider resolver for the readyz/canary probes ([`PROVIDERS_ENV`]).
///
/// `max_tokens` is caller-supplied because the two probes need very
/// different budgets: [`check_llm_ping`]'s plain "ping" needs only a few
/// tokens ([`LLM_PING_MAX_TOKENS`]), while [`check_llm_tools_ping`] must give a
/// reasoning model enough room to emit its `reasoning_content` trace before
/// the `tool_calls` payload ([`LLM_TOOLS_PING_MAX_TOKENS`]) -- see that
/// constant's doc for the root-cause writeup.
fn probe_resolver_from_env(max_tokens: u32) -> EnvEndpointResolver {
    // Probe tuning (Fable 5 diagnosis 2026-07-09): without an explicit cap,
    // the probe inherits llm.rs's defaults (max_tokens DEFAULT_MAX_TOKENS =
    // 4096, timeout 60s), so a reasoning model (v4-pro/nemotron) can burn a
    // large token budget on thinking rambling = 29-62s, blowing readyz's
    // ~25s budget. Limit one attempt to 8s, so that even a full 4-model
    // fallback walk fits the budget.
    let mut resolver = EnvEndpointResolver::new()
        .with_max_tokens(max_tokens)
        .with_request_timeout_ms(8_000)
        .with_connect_timeout_ms(3_000);
    let Ok(spec) = std::env::var(PROVIDERS_ENV) else {
        return resolver;
    };
    for entry in spec.split(';').filter(|s| !s.trim().is_empty()) {
        let parts: Vec<&str> = entry.splitn(3, '=').map(str::trim).collect();
        if let [prefix, base_url, key_field] = parts.as_slice() {
            let key_envs: Vec<String> = key_field
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect();
            if !prefix.is_empty() && !base_url.is_empty() && !key_envs.is_empty() {
                resolver = resolver.with_provider_keys(*prefix, *base_url, key_envs);
            }
        }
    }
    resolver
}

/// Cancels `needs_approval` tasks that are at least `min_age_days` old.
/// When `min_age_days == 0`, all pending approvals are cancelled (doctor --fix).
pub async fn cleanup_stale_approval_tasks(
    data_dir: &std::path::Path,
    min_age_days: i64,
) -> Result<usize> {
    use familyclaw_actions::ids::ActionTaskId;
    use familyclaw_core::time;

    let path = data_dir.join("action_tasks.jsonl");
    if !path.exists() {
        return Ok(0);
    }
    let durable = DurableTaskQueue::new(&path);
    let map = durable
        .reload()
        .await
        .map_err(|e| FamilyClawError::config(format!("action_tasks reload failed: {e}")))?;
    let now = time::now();
    let stale_ids: Vec<ActionTaskId> = map
        .iter()
        .filter(|(_, task)| task.status == TaskStatus::NeedsApproval)
        .filter(|(_, task)| now.signed_duration_since(task.updated_at).num_days() >= min_age_days)
        .map(|(id, _)| *id)
        .collect();
    if stale_ids.is_empty() {
        return Ok(0);
    }
    let queue = TaskQueue::from_map(map);
    let mut expired = 0usize;
    for id in stale_ids {
        queue
            .transition(id, TaskStatus::Cancelled, now)
            .await
            .map_err(|e| FamilyClawError::config(format!("task cancel failed: {e}")))?;
        if let Some(task) = queue.get(id).await {
            durable.append(&task).await.map_err(|e| {
                FamilyClawError::config(format!("task snapshot append failed: {e}"))
            })?;
        }
        expired += 1;
    }
    if expired > 0 {
        tracing::info!(expired, "cleaned stale needs_approval action tasks");
    }
    Ok(expired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Mutex;

    /// `std::env` on prosessinlaajuinen ja cargo ajaa testit säikeissä.
    /// Jokainen env-muuttujia koskeva testi ottaa tämän lukon ensin.
    /// Async-tietoinen mutex (ei `std::sync`), jotta lukkoa ei pidetä
    /// blokkaavana `.await`-pisteiden yli (PR #58, commit `5ec33cf`).
    static ENV_TEST_LOCK: Mutex<()> = Mutex::const_new(());

    /// Asettaa molemmat työtila-allowlistit olemassa olevaan hakemistoon ja
    /// palauttaa aiemmat arvot palautettavaksi.
    fn scope_env_set_to_existing_dir() -> (Vec<(&'static str, Option<std::ffi::OsString>)>, PathBuf)
    {
        let dir = std::env::temp_dir().join(format!("familyclaw-scope-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let previous = WORKSPACE_SCOPES
            .iter()
            .map(|(_, variable)| (*variable, std::env::var_os(variable)))
            .collect();
        for (_, variable) in WORKSPACE_SCOPES {
            std::env::set_var(variable, &dir);
        }
        (previous, dir)
    }

    fn scope_env_restore(previous: Vec<(&'static str, Option<std::ffi::OsString>)>) {
        for (variable, value) in previous {
            match value {
                Some(value) => std::env::set_var(variable, value),
                None => std::env::remove_var(variable),
            }
        }
    }

    #[test]
    fn provider_entries_are_counted_like_the_resolver_parses_them() {
        assert_eq!(count_provider_entries(""), 0);
        assert_eq!(count_provider_entries(";  ;"), 0);
        // Missing key-env field → the resolver skips it, so it does not count.
        assert_eq!(
            count_provider_entries("openai=https://api.openai.com/v1"),
            0
        );
        assert_eq!(
            count_provider_entries("openai=https://x/v1=OPENAI_API_KEY"),
            1
        );
        assert_eq!(
            count_provider_entries("a=https://x/v1=K1,K2;b=https://y/v1=K3"),
            2
        );
    }

    /// Keyless demo mode: the LLM checks are SKIPPED (not failed), so the
    /// guest path gets an honest 200 — and the skip is visible in `degraded`.
    #[tokio::test]
    async fn readyz_is_ready_and_degraded_when_llm_is_intentionally_skipped() {
        let _lock = ENV_TEST_LOCK.lock().await;
        let (previous, dir) = scope_env_set_to_existing_dir();

        let probe = ReadinessProbe {
            model: Some(ModelConfig::new("openai/gpt-4.1-mini")),
            llm_skipped: Some("no provider configured".into()),
            ..ReadinessProbe::default()
        };
        let (status, Json(body)) = deep_readyz(true, &probe).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.ready);
        assert_eq!(body.degraded, vec!["no provider configured".to_string()]);
        assert!(
            !body.checks.iter().any(|c| c.name.starts_with("llm_")),
            "llm checks must not run when skipped: {:?}",
            body.checks
        );

        scope_env_restore(previous);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The bus is still a hard gate — skipping the LLM never fakes a 200.
    #[tokio::test]
    async fn readyz_still_fails_closed_when_the_bus_is_down() {
        let probe = ReadinessProbe {
            llm_skipped: Some("no provider configured".into()),
            ..ReadinessProbe::default()
        };
        let (status, Json(body)) = deep_readyz(false, &probe).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(!body.ready);
    }

    /// Nothing skipped + nothing degraded → the JSON is byte-identical to the
    /// pre-change shape (`degraded` is omitted), so existing readiness
    /// consumers (deploy-appliance.ps1, k8s probes) keep working.
    #[tokio::test]
    async fn readyz_json_omits_degraded_when_empty() {
        let _lock = ENV_TEST_LOCK.lock().await;
        let (previous, dir) = scope_env_set_to_existing_dir();

        let probe = ReadinessProbe::default();
        let (status, Json(body)) = deep_readyz(true, &probe).await;
        assert_eq!(status, StatusCode::OK);
        let json = serde_json::to_string(&body).expect("serialize");
        assert!(!json.contains("degraded"), "{json}");

        scope_env_restore(previous);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// PR #58:sta siirretty, ilman opt-in-lippua: tyhjä allowlist tuottaa
    /// näkyvän `fail-closed`-merkinnän eikä paljasta yhtään polkua.
    #[tokio::test]
    async fn empty_workspace_scope_is_reported_fail_closed_and_redacted() {
        let _lock = ENV_TEST_LOCK.lock().await;
        let previous: Vec<_> = WORKSPACE_SCOPES
            .iter()
            .map(|(_, variable)| (*variable, std::env::var_os(variable)))
            .collect();
        for (_, variable) in WORKSPACE_SCOPES {
            std::env::remove_var(variable);
        }

        let reason = workspace_scope_check("fs_read_scope", "FAMILYCLAW_FS_READ_ALLOW")
            .expect_err("empty allowlist must not produce a check");
        assert!(reason.contains("fail-closed"), "{reason}");
        assert!(reason.contains("FAMILYCLAW_FS_READ_ALLOW"), "{reason}");

        let (status, Json(body)) = deep_readyz(true, &ReadinessProbe::default()).await;
        assert_eq!(status, StatusCode::OK, "empty allowlist is not a failure");
        assert_eq!(body.degraded.len(), 2, "{:?}", body.degraded);

        scope_env_restore(previous);
    }

    /// Tämä on se vika jonka PR #58 jätti oletuksena löytymättä: allowlist on
    /// konfiguroitu mutta osoittaa hakemistoon jota ei ole. Taito kieltää
    /// kaiken, mutta mikään ei hälytä. Nyt `/readyz` kaatuu — 503, ei 200.
    #[tokio::test]
    async fn readyz_fails_closed_when_a_configured_workspace_root_is_missing() {
        let _lock = ENV_TEST_LOCK.lock().await;
        let previous: Vec<_> = WORKSPACE_SCOPES
            .iter()
            .map(|(_, variable)| (*variable, std::env::var_os(variable)))
            .collect();
        let missing =
            std::env::temp_dir().join(format!("familyclaw-absent-{}", uuid::Uuid::new_v4()));
        assert!(!missing.is_dir(), "fixture must not exist");
        for (_, variable) in WORKSPACE_SCOPES {
            std::env::set_var(variable, &missing);
        }

        let (status, Json(body)) = deep_readyz(true, &ReadinessProbe::default()).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(!body.ready);
        for (name, _) in WORKSPACE_SCOPES {
            let check = body
                .checks
                .iter()
                .find(|c| c.name == name)
                .unwrap_or_else(|| panic!("missing check {name}: {:?}", body.checks));
            assert!(!check.ok, "{name} must fail: {}", check.detail);
            assert!(
                !check.detail.contains(&missing.display().to_string()),
                "the path must never be echoed back: {}",
                check.detail
            );
        }

        scope_env_restore(previous);
    }

    #[tokio::test]
    async fn journal_writable_check_succeeds_in_tempdir() {
        let dir = std::env::temp_dir().join(format!("familyclaw-readyz-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let result = check_journal_writable(&dir).await;
        assert!(result.ok, "{}", result.detail);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression guard for the `/readyz` `llm_tools_ping` root cause
    /// (2026-07-25): the tool-calling probe must NOT reuse the plain ping's
    /// tiny token budget. Reasoning models emit a `reasoning_content` trace
    /// before `tool_calls`, so a 32-token cap truncates the response before
    /// any tool call appears -- see [`LLM_TOOLS_PING_MAX_TOKENS`]'s doc for
    /// the empirical evidence.
    #[test]
    fn probe_resolver_gives_tools_ping_a_reasoning_safe_token_budget() {
        use familyclaw_agent::LlmEndpointResolver;

        const {
            assert!(
                // 2026-08-01: korotettu LLM_PING_MAX_TOKENS 32→512 (Kimi
                // reasoning katkesi). Tools-ping voi jakaa saman budjetin nyt.
                LLM_TOOLS_PING_MAX_TOKENS >= LLM_PING_MAX_TOKENS,
                "the tool-calling probe must not share the plain ping's token budget"
            );
        }

        let resolver = EnvEndpointResolver::new()
            .with_max_tokens(LLM_TOOLS_PING_MAX_TOKENS)
            .with_provider(
                "nvidia",
                "https://integrate.api.nvidia.com/v1",
                "DUMMY_KEY_ENV",
            );
        let cfg = resolver
            .resolve("nvidia/llama-3.3-nemotron-super-49b-v1.5")
            .expect("resolve");
        assert_eq!(cfg.max_tokens, LLM_TOOLS_PING_MAX_TOKENS);
    }
}
