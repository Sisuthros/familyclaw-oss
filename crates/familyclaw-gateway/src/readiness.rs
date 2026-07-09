//! Syvä valmiustarkistus (`/readyz`) ja kanarialintu (`POST /canary`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use axum::http::StatusCode;
use axum::Json;
use familyclaw_actions::task::{DurableTaskQueue, TaskQueue, TaskStatus};
use familyclaw_agent::llm::LlmMessage;
use familyclaw_agent::{build_llm_chain, EnvEndpointResolver};
use familyclaw_channels::DiscordChannel;
use familyclaw_core::{FamilyClawError, ModelConfig, Result};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tracing::warn;

/// Jaettu valmiusprobes-tila gatewaylle.
#[derive(Clone, Default)]
pub struct ReadinessProbe {
    /// LLM-ketju (primary + fallbacks) — sama kuin agentin runtime.
    pub model: Option<ModelConfig>,
    /// Discord-kanava webhook/bot-tilassa.
    pub discord: Option<Arc<DiscordChannel>>,
    /// Durable-journalin kirjoituskelpoinen hakemisto.
    pub data_dir: Option<PathBuf>,
}

/// Yksittäisen tarkistuksen tulos.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// Syvän `/readyz`:n JSON-vastaus.
#[derive(Debug, Clone, Serialize)]
pub struct ReadyzResponse {
    pub ready: bool,
    pub checks: Vec<CheckResult>,
}

/// Kanarialinnun JSON-vastaus.
#[derive(Debug, Clone, Serialize)]
pub struct CanaryResponse {
    pub ok: bool,
    pub latency_ms: u64,
    pub checks: Vec<CheckResult>,
}

/// Tarkistaa journal-hakemiston kirjoitettavuuden.
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

/// Kokonaisdeadline koko `llm_ping`-probelle. Per-yritys-timeout on 8s
/// (`probe_resolver_from_env`), mutta ilman KOKONAIS-deadlinea failover kävelee
/// 4 mallia x 2 pass = pahimmillaan 8x8s = 64s (audit-löytö [1]: mitattu 3.5-19.7s).
/// 20s katkaisee failover-jumin MUTTA päästää hitaan yksittäisen LLM-pingin läpi
/// (canary-mittaus 3.9-8s NIM-reitillä). watchdog.ps1 readyz-timeout on 60s, joten
/// 20s on turvallisesti sen alla eikä katkaise kelvollisia pingejä (10s oli liian
/// tiukka: ~40% pingeistä osui siihen, mitattu agent_delta 2026-07-09).
const LLM_PING_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

/// Kevyt LLM-ping (yksi lyhyt completion) — käyttää samaa provider-resolveria kuin serve.
pub async fn check_llm_ping(model_cfg: &ModelConfig) -> CheckResult {
    let resolver = probe_resolver_from_env();
    match build_llm_chain(model_cfg, &resolver) {
        Ok(chain) => {
            let messages = [LlmMessage::user("ping")];
            // TURVAKORJAUS 2026-07-09 (audit [1]): kokonaisdeadline koko
            // failover-kävelyn ympärille — muuten readyz aikakatkeaa kun useampi
            // malli timeouttaa 8s peräkkäin.
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
                    detail: format!("llm_ping deadline {}s exceeded", LLM_PING_DEADLINE.as_secs()),
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

/// Discord-gateway-yhteyden tila.
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

/// Syvä readyz: bus + LLM + Discord + journal.
pub async fn deep_readyz(
    bus_ok: bool,
    probe: &ReadinessProbe,
) -> (StatusCode, Json<ReadyzResponse>) {
    let mut checks = Vec::new();
    checks.push(CheckResult {
        name: "resonance_bus",
        ok: bus_ok,
        detail: if bus_ok { "running" } else { "not running" }.into(),
    });

    if let Some(model) = probe.model.as_ref() {
        checks.push(check_llm_ping(model).await);
    }

    if let Some(dc) = &probe.discord {
        checks.push(check_discord(dc).await);
    }

    if let Some(dir) = &probe.data_dir {
        checks.push(check_journal_writable(dir).await);
    }

    let ready = checks.iter().all(|c| c.ok);
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(ReadyzResponse { ready, checks }))
}

/// Kanarialintu: synteettinen LLM-vuoro + infratarkistukset.
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

/// Rakentaa valmiusproben serve()-käynnistyksessä.
pub fn build_probe(
    model: Option<ModelConfig>,
    discord: Option<Arc<DiscordChannel>>,
) -> ReadinessProbe {
    let data_dir = std::env::var("FAMILYCLAW_DATA_DIR").ok().map(PathBuf::from);
    ReadinessProbe {
        model,
        discord,
        data_dir,
    }
}

/// Provider-resolver readyz/canary-probeille (`FAMILYCLAW_PROVIDERS`).
fn probe_resolver_from_env() -> EnvEndpointResolver {
    const PROVIDERS_ENV: &str = "FAMILYCLAW_PROVIDERS";
    // Probe-viritys (Fable 5 -diagnoosi 2026-07-09): ping tarvitsee vain pari
    // tokenia. Ilman näitä probe perii llm.rs:n oletukset (max_tokens 2048,
    // timeout 60s), jolloin reasoning-malli (v4-pro/nemotron) polttaa koko 2048
    // tokenin budjetin thinking-jaaritteluun "ping"-viestille = 29-62s, ja
    // readyz:n ~25s budjetti ylittyy. Katkaise reasoning 32 tokeniin ja rajaa
    // yksi yritys 8s:iin, jotta täysi 4 mallin fallback-kävelykin mahtuu budjettiin.
    let mut resolver = EnvEndpointResolver::new()
        .with_max_tokens(32)
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

/// Peruuttaa `needs_approval`-tehtävät jotka ovat vähintään `min_age_days` vanhoja.
/// Kun `min_age_days == 0`, peruutetaan kaikki odottavat hyväksynnät (doctor --fix).
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

    #[tokio::test]
    async fn journal_writable_check_succeeds_in_tempdir() {
        let dir = std::env::temp_dir().join(format!("familyclaw-readyz-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let result = check_journal_writable(&dir).await;
        assert!(result.ok, "{}", result.detail);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
