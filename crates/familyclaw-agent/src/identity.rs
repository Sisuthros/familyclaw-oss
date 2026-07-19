//! Operator identity guard and brief-ping fast replies (Layer A).
//!
//! Prevents wrong-name addressing when semantic recall pulls roleplay/channel
//! fiction into operator DMs. Uses `FAMILYCLAW_OWNER_ID` only — no private names.

use std::path::{Path, PathBuf};

use familyclaw_bus::{BusMessage, MessageOrigin};
use serde_json::{json, Value};

/// Reads designated operator user id from `FAMILYCLAW_OWNER_ID`.
#[must_use]
pub fn operator_id() -> Option<u64> {
    std::env::var("FAMILYCLAW_OWNER_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&id| id > 0)
}

/// `true` when the message origin sender matches `FAMILYCLAW_OWNER_ID`.
#[must_use]
pub fn is_operator_origin(origin: Option<&MessageOrigin>) -> bool {
    let Some(op) = operator_id() else {
        return false;
    };
    let Some(origin) = origin else {
        return false;
    };
    origin.sender.parse::<u64>().ok() == Some(op)
}

/// Memory tag for the external sender id (Discord user snowflake, etc.).
#[must_use]
pub fn peer_tag(sender: &str) -> String {
    format!("peer:{sender}")
}

/// Memory tag marking operator-scoped recall.
#[must_use]
pub fn scope_operator_tag() -> &'static str {
    "scope:operator"
}

/// System-prompt guard block for operator messages.
#[must_use]
pub fn identity_guard_prompt(origin: Option<&MessageOrigin>) -> String {
    if !is_operator_origin(origin) {
        return String::new();
    }
    [
        "",
        "[OPERATOR IDENTITY GUARD]",
        "The current speaker is the designated human operator.",
        "Do NOT address them using names from roleplay, channel fiction, or recalled memories.",
        "If they state their name, trust that over memory. Never call them a sibling persona name.",
        "Keep replies concise when they send a short ping.",
        "",
        "[OPERATOR CAPABILITY RULES]",
        "When they ask you to DO something or answer a substantive question: answer it directly.",
        "Use tools (fs_read, file_write, web_search, research) on the current request when helpful.",
        "Do NOT call shell_exec for normal analysis or file reading; use fs_read_allowlisted instead.",
        "When reading files for operator work, call fs_read with read_full_content: true.",
        "Only use shell_exec when the operator explicitly asks for a shell command.",
        "[TOOL CONTRACT]",
        "Tools are the ONLY way to read/write disk or search the web. Never claim a file was written or read unless a tool_result in THIS turn confirms it.",
        "If you have not called tools yet, say what you would do next — do not invent evidence paths or DONE lines.",
        "For operator diagnostics, use technical style: concise bullets, concrete issues, concrete fixes.",
        "Do NOT use roleplay/family prose in operator diagnostics (no sibling narrative, no emotional framing).",
        "When asked what is missing/failing, provide prioritized list (P0/P1/P2) with actionable items.",
        "Do NOT end diagnostics with open-ended prompts like 'Oletko samaa mieltä?' or 'Mitä seuraavaksi?'.",
        "Never end with 'what do you need next', 'mitä seuraavaksi', or empty tool boasts.",
        "If they express frustration, acknowledge briefly and deliver the requested output — do not ask clarifying questions first.",
        "[END GUARD]",
    ]
    .join("\n")
}

/// Drops recalled memories that look like third-party roleplay noise for operator turns.
#[must_use]
pub fn filter_memories_for_operator<T>(
    memories: Vec<T>,
    origin: Option<&MessageOrigin>,
    content: impl Fn(&T) -> &str,
) -> Vec<T> {
    if !is_operator_origin(origin) {
        return memories;
    }
    memories
        .into_iter()
        .filter(|m| !memory_is_roleplay_noise(content(m)))
        .filter(|m| !crate::grounding::memory_is_unverified_tool_claim(content(m)))
        .collect()
}

/// Generic substrings that mark a recalled memory as third-party roleplay
/// noise rather than genuine operator context (e.g. a sibling-persona voice
/// bleeding into an operator DM). These are deliberately generic examples;
/// there is no configuration hook today for deployments to supply their own
/// patterns, so real deployments should extend this list in code rather than
/// relying on runtime config.
const DEFAULT_ROLEPLAY_NOISE_PATTERNS: &[&str] = &[
    "your sibling ",
    "sibling here",
    "— agent_b",
    "agent_b here",
    "agent_b speaking",
    "your sister",
    "cronjob response:",
];

fn memory_is_roleplay_noise(content: &str) -> bool {
    let lower = content.to_lowercase();
    DEFAULT_ROLEPLAY_NOISE_PATTERNS
        .iter()
        .any(|m| lower.contains(m))
}

/// `true` for short agent-name pings like "`agent_a`?!" (≤4 words, ≤48 chars).
#[must_use]
pub fn is_brief_ping(query: &str, agent_name: &str) -> bool {
    let t = query.trim();
    if t.is_empty() || t.len() > 48 {
        return false;
    }
    if t.split_whitespace().count() > 4 {
        return false;
    }
    let lower = t.to_lowercase();
    let name = agent_name.to_lowercase();
    if !lower.contains(&name) {
        return false;
    }
    let alnum: String = lower.chars().filter(|c| c.is_alphanumeric()).collect();
    let name_alnum: String = name.chars().filter(|c| c.is_alphanumeric()).collect();
    alnum.len() <= name_alnum.len().saturating_add(6)
}

/// Fast ack for brief pings — skips LLM essay.
#[must_use]
pub fn brief_ping_reply(agent_name: &str, message: &BusMessage) -> Option<String> {
    let query = match message {
        BusMessage::Text { body } => body.as_str(),
        BusMessage::Latent { text_shadow, .. } => text_shadow.as_str(),
        _ => return None,
    };
    if !is_brief_ping(query, agent_name) {
        return None;
    }
    Some(format!("Tässä! ✦ ({agent_name} kuulee — mitä tarvitset?)"))
}

/// `true` when the operator explicitly asks for a tool invocation — must not use
/// canned diagnostic fast paths (e.g. TOP 20 summary vs. `fs_read` on `top20.md`).
#[must_use]
fn explicit_tool_request(query: &str) -> bool {
    let q = query.trim().to_lowercase();
    [
        "fs_read",
        "file_write",
        "web_search",
        "web_fetch",
        "shell_exec",
        "read_full_content",
        "käytä työkalu",
        "käytä fs_read",
        "use fs_read",
        "use tool",
        "älä keksi",
        "don't make up",
        "do not invent",
    ]
    .iter()
    .any(|needle| q.contains(needle))
        || q.starts_with("lue ")
        || (q.contains("lue ") && q.contains(".md"))
}

/// Fast technical reply for operator efficiency/diagnostic prompts.
#[must_use]
pub fn operator_diagnostic_reply(
    message: &BusMessage,
    origin: Option<&MessageOrigin>,
) -> Option<String> {
    if !is_operator_origin(origin) {
        return None;
    }
    let query = match message {
        BusMessage::Text { body } => body.as_str(),
        BusMessage::Latent { text_shadow, .. } => text_shadow.as_str(),
        _ => return None,
    };
    if explicit_tool_request(query) {
        return None;
    }
    let q = query.trim().to_lowercase();
    if q.contains("minne siirrät") || q.contains("where are you moving") {
        return Some(
            "Moving research materials to this path:\n- /data/agent_home/research/legacy/2026-07\n\nGrowth-related legacy files go here:\n- /data/agent_home/growth/legacy/2026-07\n\nI will not move files containing secrets (.env, secrets).".to_string(),
        );
    }
    if q.contains("pystyt nyt toimimaan")
        || q.contains("can you work now")
        || q.contains("are you working now")
        || q.contains("toimitko")
        || q.contains("toimit?")
    {
        return None;
    }
    if q.contains("miten sinusta saadaan")
        || q.contains("kunnolla toimiva")
        || q.contains("how to make you work")
    {
        return Some(operator_how_to_work_reply());
    }
    if !(q.contains("miksi et toimi tehokkaasti")
        || q.contains("mikä pitää vielä korjata")
        || q.contains("top 20")
        || q.contains("familyclawssa"))
    {
        return None;
    }
    Some(
        "P0\n- turn-timeoutit ja fallback-polku täysin deterministisiksi (ei tyhjää vastausta)\n- shell_exec-väärinkäyttö pois analyysistä (fs_read ensisijainen)\n- typing katkeaa heti timeout/error-tilassa\n\nP1\n- vastaukset aina teknisinä prioriteetteina (ei roolipeliä)\n- työkalukutsuista suora tulos + todiste, ei metapuhetta\n- approval-jumit auto-siivoukseen hallitusti\n\nP2\n- yhtenäinen tutkimusartefakti-pohja (tiivistelmä, vertailu, next action)\n- regressiotestit: väärä persona, tyhjä vastaus, shell_exec-looppi\n- observability: lokiin selkeä malli/fallback-syy per vuoro".to_string(),
    )
}

/// First allowlisted workspace root (`FAMILYCLAW_FILE_WRITE_ALLOW` or read allow).
#[must_use]
pub fn operator_home_root() -> Option<PathBuf> {
    let raw = std::env::var("FAMILYCLAW_FILE_WRITE_ALLOW")
        .or_else(|_| std::env::var("FAMILYCLAW_FS_READ_ALLOW"))
        .ok()?;
    std::env::split_paths(&raw).next()
}

/// `true` for operator status checks ("Toimitko?", "status", …) — runs disk-backed bootstrap.
#[must_use]
pub fn operator_status_message(message: &BusMessage, origin: Option<&MessageOrigin>) -> bool {
    if !is_operator_origin(origin) {
        return false;
    }
    let query = match message {
        BusMessage::Text { body } => body.as_str(),
        BusMessage::Latent { text_shadow, .. } => text_shadow.as_str(),
        _ => return false,
    };
    let q = query.trim().to_lowercase();
    q.contains("toimitko")
        || q.contains("toimit?")
        || q == "status"
        || q.contains("status report")
        || q.contains("pystyt nyt")
        || q.contains("are you working")
}

/// Parses operator `APPROVE <uuid>` / `DENY <uuid>` chat commands.
#[must_use]
pub fn parse_operator_approval_command(message: &BusMessage) -> Option<OperatorApprovalCommand> {
    let query = match message {
        BusMessage::Text { body } => body.trim(),
        BusMessage::Latent { text_shadow, .. } => text_shadow.trim(),
        _ => return None,
    };
    let lower = query.to_lowercase();
    let rest = if lower.strip_prefix("approve ").is_some() {
        Some(("approve", query[8..].trim()))
    } else if lower.strip_prefix("deny ").is_some() {
        Some(("deny", query[5..].trim()))
    } else if lower.strip_prefix("hyväksy ").is_some() {
        Some(("approve", query[8..].trim()))
    } else if lower.strip_prefix("hylkää ").is_some() {
        Some(("deny", query[7..].trim()))
    } else {
        None
    };
    let (kind, id_str) = rest?;
    if id_str.len() < 8 {
        return None;
    }
    if kind == "approve" {
        Some(OperatorApprovalCommand::Approve(id_str.to_string()))
    } else {
        Some(OperatorApprovalCommand::Deny(id_str.to_string()))
    }
}

/// Operator chat gateway command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorApprovalCommand {
    /// Approve pending action by approval UUID.
    Approve(String),
    /// Deny pending action by approval UUID.
    Deny(String),
}

/// Builds a factual status report from canonical home files (no LLM).
#[must_use]
pub fn operator_status_report(home: &Path) -> String {
    let files = [
        ("memory.md", home.join("memory.md")),
        (
            "priorities/top20.md",
            home.join("priorities").join("top20.md"),
        ),
        ("research/log.md", home.join("research").join("log.md")),
    ];
    let mut lines = vec![
        "STATUS (levyltä, ei LLM)".to_string(),
        format!("Home: `{}`", home.display()),
        String::new(),
    ];
    for (label, path) in files {
        if path.exists() {
            let meta = std::fs::metadata(&path).ok();
            let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
            let mtime = meta.and_then(|m| m.modified().ok()).map_or_else(
                || "?".to_string(),
                |t| {
                    let dt: chrono::DateTime<chrono::Utc> = t.into();
                    dt.format("%Y-%m-%d %H:%M UTC").to_string()
                },
            );
            lines.push(format!("- `{label}`: {size} B, mtime {mtime}"));
        } else {
            lines.push(format!("- `{label}`: **puuttuu**"));
        }
    }
    lines.push(String::new());
    lines.push(
        "Jos tarvitset työn: `Tee se!` (TOP20 bootstrap) tai anna konkreettinen tehtävä (käytän työkaluja)."
            .to_string(),
    );
    lines.join("\n")
}

/// `true` when operator sends execute-now phrasing ("Tee se!", "JATKA", …).
#[must_use]
pub fn operator_execute_message(message: &BusMessage, origin: Option<&MessageOrigin>) -> bool {
    if !is_operator_origin(origin) {
        return false;
    }
    let query = match message {
        BusMessage::Text { body } => body.as_str(),
        BusMessage::Latent { text_shadow, .. } => text_shadow.as_str(),
        _ => return false,
    };
    let q = query.trim().to_lowercase();
    is_operator_go_ahead(&q) || is_operator_continue(&q)
}

fn is_operator_go_ahead(q: &str) -> bool {
    let t = q.trim().trim_end_matches(['.', '!']);
    let lower = t.to_lowercase();
    [
        "tee se",
        "tee vaan",
        "aloita",
        "do it",
        "go ahead",
        "start now",
        "hyväksyn",
        "hyväksyn anna palaa",
        "anna palaa",
        "anna palaa!",
    ]
    .iter()
    .any(|p| lower == *p || lower.starts_with(&format!("{p} ")))
}

fn is_operator_continue(q: &str) -> bool {
    let t = q.trim().trim_end_matches(['.', '!']);
    matches!(
        t.to_lowercase().as_str(),
        "jatka" | "continue" | "resume" | "jatka!"
    )
}

/// After approval suspend — explain gateway path + immediate fallback work.
#[must_use]
pub fn operator_continue_reply() -> String {
    [
        "Jatkan — huom: chat-'hyväksyn' ei hyväksy file_patch_apply:ä.",
        "Odottava tehtävä vaatii gateway-hyväksynnän (`/approvals`).",
        "",
        "Teen nyt ilman patchia (TOP 20 #1):",
        "- päivitän `home/memory.md`",
        "- kirjaa `home/research/log.md`",
        "- käytän fs_read `read_full_content: true` TOP20:een",
    ]
    .join("\n")
}

/// Honest capability answer — no fabricated patches.
#[must_use]
pub fn operator_how_to_work_reply() -> String {
    [
        "P0 — mitä tarvitaan että toimin kunnolla:",
        "1. fs_read `read_full_content: true` (nyt tuettu) → TOP20 + memory luettavissa",
        "2. file_write allowlistissa `home/` — ei shell_exec-moveja",
        "3. Approvalit gatewayssä (`/approvals`), ei pelkkä chat-hyväksyntä",
        "4. Yksi vastaus per pyyntö — ei työkaluspämmiä",
        "",
        "Seuraava askel: päivitän research/log.md + memory.md TOP 20 #1:llä.",
    ]
    .join("\n")
}

/// File-write payloads for TOP 20 #1 bootstrap (`file_write_allowlisted`).
#[must_use]
pub fn operator_top20_bootstrap_plan(home: &Path, now_iso: &str) -> Vec<(String, Value)> {
    let memory_path = home.join("memory.md");
    let log_path = home.join("research").join("log.md");
    let memory_entry = format!(
        "\n## {now_iso}\n- DONE: TOP 20 #1 operator bootstrap (file_write)\n- Canonical memory: `{}`\n- Research log: `{}`\n",
        memory_path.display(),
        log_path.display()
    );
    let log_entry = format!(
        "\n## {now_iso}\n- DONE: TOP 20 #1 memory bootstrap\n- Updated: `memory.md`\n- Operator command: Tee se / JATKA\n"
    );
    vec![
        (
            memory_path.display().to_string(),
            json!({
                "path": memory_path.display().to_string(),
                "content": memory_entry,
                "mode": "append"
            }),
        ),
        (
            log_path.display().to_string(),
            json!({
                "path": log_path.display().to_string(),
                "content": log_entry,
                "mode": "append"
            }),
        ),
    ]
}

/// Reply after successful bootstrap writes.
#[must_use]
pub fn operator_top20_bootstrap_done_reply(home: &Path, written: &[String]) -> String {
    let memory = home.join("memory.md");
    let log = home.join("research").join("log.md");
    format!(
        "DONE: TOP 20 #1 — muistijärjestelmä päivitetty.\n\nKirjoitetut tiedostot:\n{}\n\nTodiste:\n- `{}`\n- `{}`\n\nSeuraava: yksi `web_search` → tallennus `home/research/<aihe>/report.md`.",
        written
            .iter()
            .map(|p| format!("- `{p}`"))
            .collect::<Vec<_>>()
            .join("\n"),
        memory.display(),
        log.display()
    )
}

/// Reply when bootstrap could not complete (approval block, write error).
#[must_use]
pub fn operator_top20_bootstrap_blocked_reply(reason: &str) -> String {
    format!(
        "BLOCKED: TOP 20 #1 bootstrap — {reason}\nNEXT: hyväksy odottavat tehtävät gatewayssä (`/approvals`) tai aja `familyclaw-gateway doctor --fix`."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_bus::MessageOrigin;
    use std::sync::{Mutex, MutexGuard};

    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn env_test_lock() -> MutexGuard<'static, ()> {
        ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn brief_ping_detects_short_call() {
        assert!(is_brief_ping("agent_a?!", "agent_a"));
        assert!(is_brief_ping("agent_a?", "agent_a"));
        assert!(!is_brief_ping(
            "agent_a, read your SOUL and write a full report",
            "agent_a"
        ));
    }

    #[test]
    fn operator_identity_guard_and_recall_filter() {
        const ENV: &str = "FAMILYCLAW_OWNER_ID";
        let _lock = env_test_lock();
        let prior = std::env::var(ENV).ok();
        std::env::set_var(ENV, "42");
        let origin = MessageOrigin::new("discord-1", "100", "42");
        let memories = vec![
            "normal operator question".to_string(),
            "Hey agent_a! Your sibling agent_b here.".to_string(),
        ];
        let filtered = filter_memories_for_operator(memories, Some(&origin), |s| s.as_str());
        assert_eq!(filtered.len(), 1);
        assert!(filtered[0].contains("normal"));
        assert!(identity_guard_prompt(Some(&origin)).contains("OPERATOR IDENTITY GUARD"));
        let other = MessageOrigin::new("c", "conv", "1");
        assert!(identity_guard_prompt(Some(&other)).is_empty());
        match prior {
            Some(v) => std::env::set_var(ENV, v),
            None => std::env::remove_var(ENV),
        }
    }

    #[test]
    fn operator_diagnostic_fast_paths_cover_direct_questions() {
        const ENV: &str = "FAMILYCLAW_OWNER_ID";
        let _lock = env_test_lock();
        let prior = std::env::var(ENV).ok();
        std::env::set_var(ENV, "42");
        let origin = MessageOrigin::new("discord-1", "100", "42");
        let moved = operator_diagnostic_reply(&BusMessage::text("Minne siirrät?"), Some(&origin))
            .expect("direct move question should be fast-pathed");
        assert!(moved.contains("/data/agent_home/research/legacy/2026-07"));
        let can_work =
            operator_status_message(&BusMessage::text("Pystyt nyt toimimaan?"), Some(&origin));
        assert!(can_work);
        assert!(
            operator_diagnostic_reply(&BusMessage::text("Pystyt nyt toimimaan?"), Some(&origin))
                .is_none(),
            "status questions must use disk bootstrap, not text fast path"
        );
        assert!(
            operator_diagnostic_reply(&BusMessage::text("Tee se!"), Some(&origin)).is_none(),
            "go-ahead must run bootstrap tools, not text-only fast path"
        );
        assert!(
            operator_diagnostic_reply(&BusMessage::text("JATKA."), Some(&origin)).is_none(),
            "continue must run bootstrap tools, not text-only fast path"
        );
        assert!(
            operator_diagnostic_reply(
                &BusMessage::text(
                    "Lue priorities/top20.md read_full_content true ja kerro mikä on TOP 20 #1. Älä keksi — käytä fs_read."
                ),
                Some(&origin),
            )
            .is_none(),
            "explicit fs_read must reach tool loop, not TOP20 diagnostic fast path"
        );
        assert!(operator_execute_message(
            &BusMessage::text("Tee se!"),
            Some(&origin)
        ));
        assert!(operator_execute_message(
            &BusMessage::text("JATKA."),
            Some(&origin)
        ));
        let plan =
            operator_top20_bootstrap_plan(Path::new(r"E:\agent_home"), "2026-07-14T12:00:00Z");
        assert_eq!(plan.len(), 2);
        assert!(plan[0].1.get("mode").and_then(|v| v.as_str()) == Some("append"));
        let how = operator_diagnostic_reply(
            &BusMessage::text("Miten sinusta saadaan kunnolla toimiva!"),
            Some(&origin),
        )
        .expect("how-to-work should be fast-pathed");
        assert!(how.contains("read_full_content"));
        match prior {
            Some(v) => std::env::set_var(ENV, v),
            None => std::env::remove_var(ENV),
        }
    }
}
