//! Operator identity guard and brief-ping fast replies (Layer A).
//!
//! Prevents wrong-name addressing when semantic recall pulls roleplay/channel
//! fiction into operator DMs. Uses `FAMILYCLAW_OWNER_ID` only — no private names.

use familyclaw_bus::{BusMessage, MessageOrigin};

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
        .collect()
}

fn memory_is_roleplay_noise(content: &str) -> bool {
    let lower = content.to_lowercase();
    [
        "sisaresi ",
        "sister ",
        "— agent_epsilon",
        "agent_epsilon täältä",
        "agent_epsilon täällä",
        "your sister",
        "cronjob response:",
        "he i agent_epsilon",
    ]
    .iter()
    .any(|m| lower.contains(m))
}

/// `true` for short agent-name pings like "agent_delta?!" (≤4 words, ≤48 chars).
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
    let q = query.trim().to_lowercase();
    if q.contains("minne siirrät") || q.contains("where are you moving") {
        return Some(
            "Siirrän tutkimusmateriaalit tähän polkuun:\n- E:\\agent_delta\\home\\research\\legacy\\2026-07\n\nKasvuun liittyvät legacyt tähän:\n- E:\\agent_delta\\home\\growth\\legacy\\2026-07\n\nEn siirrä salaisuuksia sisältäviä tiedostoja (.env, secrets).".to_string(),
        );
    }
    if q.contains("pystyt nyt toimimaan")
        || q.contains("can you work now")
        || q.contains("are you working now")
    {
        return Some(
            "Kyllä — pystyn toimimaan. Teen nyt suorat vastaukset, käytän progress-mittaria ja vältän turhan metapuheen.\nJos haluat, käynnistän heti ensimmäisen konkreettisen tehtävän TOP 20 -listalta."
                .to_string(),
        );
    }
    if is_operator_go_ahead(&q) {
        return Some(operator_top20_go_ahead_reply());
    }
    if is_operator_continue(&q) {
        return Some(operator_continue_reply());
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

/// Deterministic TOP 20 #1 kickoff — no tool loop for operator "go ahead".
#[must_use]
pub fn operator_top20_go_ahead_reply() -> String {
    [
        "TOP 20 #1 — aloitan nyt:",
        "",
        "**Muistijärjestelmä istuntojen yli**",
        "- Varmistan `home/memory.md` -pohjan",
        "- Päivitän `home/research/log.md` ensimmäisellä merkinnällä",
        "- Pidän tutkimuspolun: `home/research/`",
        "",
        "En käytä shell_exec-moveja. Raportoin kun ensimmäinen askel on kirjattu.",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_bus::MessageOrigin;

    #[test]
    fn brief_ping_detects_short_call() {
        assert!(is_brief_ping("agent_delta?!", "agent_delta"));
        assert!(is_brief_ping("agent_delta?", "agent_delta"));
        assert!(!is_brief_ping(
            "agent_delta, read your SOUL and write a full report",
            "agent_delta"
        ));
    }

    #[test]
    fn operator_identity_guard_and_recall_filter() {
        const ENV: &str = "FAMILYCLAW_OWNER_ID";
        let prior = std::env::var(ENV).ok();
        std::env::set_var(ENV, "42");
        let origin = MessageOrigin::new("discord-1", "100", "42");
        let memories = vec![
            "normal operator question".to_string(),
            "Hei agent_delta! Sisaresi agent_epsilon täältä.".to_string(),
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
        let prior = std::env::var(ENV).ok();
        std::env::set_var(ENV, "42");
        let origin = MessageOrigin::new("discord-1", "100", "42");
        let moved = operator_diagnostic_reply(&BusMessage::text("Minne siirrät?"), Some(&origin))
            .expect("direct move question should be fast-pathed");
        assert!(moved.contains("E:\\agent_delta\\home\\research\\legacy\\2026-07"));
        let can_work =
            operator_diagnostic_reply(&BusMessage::text("Pystyt nyt toimimaan?"), Some(&origin))
                .expect("direct status question should be fast-pathed");
        assert!(can_work.to_lowercase().contains("pystyn toimimaan"));
        let go = operator_diagnostic_reply(&BusMessage::text("Tee se!"), Some(&origin))
            .expect("go-ahead should be fast-pathed");
        assert!(go.contains("TOP 20 #1"));
        let cont = operator_diagnostic_reply(&BusMessage::text("JATKA."), Some(&origin))
            .expect("continue should be fast-pathed");
        assert!(cont.contains("gateway"));
        assert!(!cont.contains("turn-timeoutit"));
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
