//! Hallucination guard and tool escalation (Layer A).

/// `true` when the message expects concrete action / a status check rather than small talk.
#[must_use]
pub fn looks_like_action_request(query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() || q.len() > 512 {
        return false;
    }
    if q.split_whitespace().count() <= 2 {
        return matches!(
            q.as_str(),
            "toimitko?"
                | "toimitko"
                | "toimit?"
                | "status"
                | "raportti"
                | "report"
                | "working?"
                | "are you working"
        ) || q.contains("toimitko")
            || q.contains("pystytkö")
            || q.contains("pystyt nyt");
    }
    [
        "tee ",
        "kirjoita",
        "lue ",
        "read ",
        "write ",
        "tutki",
        "research",
        "web_search",
        "file_write",
        "fs_read",
        "top 20",
        "top20",
        "memory",
        "muisti",
        "korjaa",
        "fix ",
        "deploy",
        "analysoi",
        "analyze",
    ]
    .iter()
    .any(|needle| q.contains(needle))
}

/// `true` when the response claims tool activity without journal proof.
#[must_use]
pub fn response_claims_tool_use(text: &str) -> bool {
    let lower = text.to_lowercase();
    if lower.contains("⚠") && lower.contains("vahvistamaton") {
        return false;
    }
    let claim_markers = [
        "done:",
        "✅",
        "kirjoitin",
        "tallensin",
        "luotu",
        "generated",
        "updated:",
        "päivitetty",
        "evidence:",
        "todiste:",
        "auto-updated",
        "functional",
        "operational",
        "fs_read_allowlisted",
        "file_write_allowlisted",
        "hyperframes",
        "agent_ledger",
        "hermes",
        "cron",
    ];
    if !claim_markers.iter().any(|m| lower.contains(m)) {
        return false;
    }
    // A path combined with an action claim together is a strong signal.
    lower.contains(":\\") || lower.contains("e:/") || lower.contains("e:\\\\")
}

/// Adds a warning if the response claims work was done but the dispatch count is zero.
#[must_use]
pub fn apply_grounding_guard(answer: &str, dispatch_count: u32) -> String {
    if dispatch_count > 0 || !response_claims_tool_use(answer) {
        return answer.to_string();
    }
    format!(
        "{answer}\n\n⚠ **Vahvistamaton:** Tässä vuorossa ei ajettu yhtään työkalua (dispatch=0). \
         Yllä olevat teko-väitteet eivät ole journal-todistettuja."
    )
}

/// Memory entries that likely contain prior action claims (RAG filter).
#[must_use]
pub fn memory_is_unverified_tool_claim(content: &str) -> bool {
    let lower = content.to_lowercase();
    [
        "agent_ledger",
        "hyperframes_integration",
        "research_log.json",
        "hermes cron",
        "hermes-cron",
        "auto-updated with research",
        "build execution",
        "operator status report",
        "executing full agent harness",
    ]
    .iter()
    .any(|m| lower.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_request_detects_operator_status() {
        assert!(looks_like_action_request("Toimitko?"));
        assert!(looks_like_action_request("Tee se nyt"));
        assert!(!looks_like_action_request("Hei, mitä kuuluu?"));
    }

    #[test]
    fn grounding_guard_flags_hallucinated_done() {
        let text = "DONE: updated E:\\Agent\\home\\memory.md";
        let out = apply_grounding_guard(text, 0);
        assert!(out.contains("Vahvistamaton"));
        assert_eq!(apply_grounding_guard(text, 1), text);
    }
}
