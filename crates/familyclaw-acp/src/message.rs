//! ACP-viestityypit — JSON-serialisoitavat request/response-rakenteet.
//!
//! ACP (Agent Communication Protocol) käyttää JSON-RPC 2.0 -tyyppistä
//! viestimuotoa stdin/stdout:n yli. Tämä moduuli määrittelee protokollan
//! kannalta olennaiset tietorakenteet.

use serde::{Deserialize, Serialize};

/// ACP-kutsu agentille.
///
/// Vastaa käyttäjän promptia: "tee tämä asia".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpRequest {
    /// Prompt-teksti agentille.
    pub prompt: String,
    /// Käyttöoikeustila (esim. "default", "`bypass_permissions`").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// Työhakemisto.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
}

impl AcpRequest {
    /// Luo uuden ACP-kutsun promptilla.
    #[must_use]
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            permission_mode: None,
            working_dir: None,
        }
    }

    /// Asettaa käyttöoikeustilan.
    #[must_use]
    pub fn with_permission_mode(mut self, mode: impl Into<String>) -> Self {
        self.permission_mode = Some(mode.into());
        self
    }
}

/// ACP-vastaus agentilta.
///
/// Agentti palauttaa tekstimuotoisen vastauksen promptiin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpResponse {
    /// Agentin tekstivastaus.
    pub content: String,
    /// Työkalukutsut joita agentti teki (tiedostojen luku, shell-komennot jne.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<AcpToolCall>,
    /// Kuinka monta tokenia käytettiin (jos agentti raportoi).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<AcpTokenUsage>,
    /// Sessio-ID jatkuvuutta varten.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// Agentin tekemä työkalukutsu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpToolCall {
    /// Työkalun nimi (esim. "`read_file`", "`execute_command`").
    pub tool: String,
    /// Työkalun argumentit JSON-muodossa.
    pub arguments: serde_json::Value,
    /// Työkalun palauttama tulos (jos saatavissa).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

/// Työkalun palauttama tulos agentille.
///
/// Lähetetään takaisin agentille kun työkalu on suoritettu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpToolResult {
    /// Työkalukutsun ID johon tämä on vastaus.
    pub tool_call_id: String,
    /// Työkalun tulostama sisältö.
    pub content: String,
    /// Oliko suoritus onnistunut.
    #[serde(default = "default_success")]
    pub success: bool,
}

fn default_success() -> bool {
    true
}

/// Token-käyttötilasto.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AcpTokenUsage {
    /// Syötetokenien määrä.
    pub input_tokens: u32,
    /// Tulostetokenien määrä.
    pub output_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_new_defaults_options_to_none() {
        let req = AcpRequest::new("prompt_text");
        assert_eq!(req.prompt, "prompt_text");
        assert!(req.permission_mode.is_none());
        assert!(req.working_dir.is_none());
    }

    #[test]
    fn request_with_permission_mode_sets_some() {
        let req = AcpRequest::new("prompt_text").with_permission_mode("default");
        assert_eq!(req.permission_mode.as_deref(), Some("default"));
    }

    #[test]
    fn request_serialize_omits_none_options() {
        // permission_mode + working_dir = None → kentät jätetään pois (skip_serializing_if).
        let req = AcpRequest::new("prompt_text");
        let value: serde_json::Value = serde_json::to_value(&req).expect("serialize request");
        let obj = value.as_object().expect("request is an object");
        assert!(obj.contains_key("prompt"));
        assert!(!obj.contains_key("permission_mode"));
        assert!(!obj.contains_key("working_dir"));
    }

    #[test]
    fn request_serialize_includes_some_options() {
        let req = AcpRequest::new("prompt_text").with_permission_mode("bypass_permissions");
        let value: serde_json::Value = serde_json::to_value(&req).expect("serialize request");
        let obj = value.as_object().expect("request is an object");
        assert_eq!(obj["permission_mode"], serde_json::json!("bypass_permissions"));
    }

    #[test]
    fn response_serialize_omits_empty_tool_calls_and_none_fields() {
        // tool_calls tyhjä Vec + token_usage/session_id None → kaikki jätetään pois.
        let resp = AcpResponse {
            content: "hello".to_string(),
            tool_calls: Vec::new(),
            token_usage: None,
            session_id: None,
        };
        let value: serde_json::Value = serde_json::to_value(&resp).expect("serialize response");
        let obj = value.as_object().expect("response is an object");
        assert!(obj.contains_key("content"));
        assert!(!obj.contains_key("tool_calls"));
        assert!(!obj.contains_key("token_usage"));
        assert!(!obj.contains_key("session_id"));
    }

    #[test]
    fn response_serialize_includes_non_empty_tool_calls() {
        let resp = AcpResponse {
            content: "done".to_string(),
            tool_calls: vec![AcpToolCall {
                tool: "read_file".to_string(),
                arguments: serde_json::json!({"path": "a.txt"}),
                result: None,
            }],
            token_usage: Some(AcpTokenUsage {
                input_tokens: 10,
                output_tokens: 20,
            }),
            session_id: Some("session_a".to_string()),
        };
        let value: serde_json::Value = serde_json::to_value(&resp).expect("serialize response");
        let obj = value.as_object().expect("response is an object");
        assert_eq!(obj["tool_calls"].as_array().expect("array").len(), 1);
        assert_eq!(obj["session_id"], serde_json::json!("session_a"));
    }

    #[test]
    fn response_deserialize_missing_tool_calls_becomes_empty_vec() {
        // tool_calls puuttuu → #[serde(default)] tekee tyhjän Vecin.
        let json = r#"{"content": "hi"}"#;
        let resp: AcpResponse = serde_json::from_str(json).expect("deserialize response");
        assert_eq!(resp.content, "hi");
        assert!(resp.tool_calls.is_empty());
        assert!(resp.token_usage.is_none());
        assert!(resp.session_id.is_none());
    }

    #[test]
    fn response_deserialize_full_payload_roundtrips() {
        let json = r#"{
            "content": "result",
            "tool_calls": [
                {"tool": "execute_command", "arguments": {"cmd": "ls"}, "result": "ok"}
            ],
            "token_usage": {"input_tokens": 5, "output_tokens": 7},
            "session_id": "session_a"
        }"#;
        let resp: AcpResponse = serde_json::from_str(json).expect("deserialize response");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].tool, "execute_command");
        assert_eq!(resp.tool_calls[0].result.as_deref(), Some("ok"));
        let usage = resp.token_usage.expect("usage present");
        assert_eq!(usage.input_tokens, 5);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(resp.session_id.as_deref(), Some("session_a"));
    }

    #[test]
    fn tool_call_serialize_omits_none_result() {
        let call = AcpToolCall {
            tool: "read_file".to_string(),
            arguments: serde_json::json!({}),
            result: None,
        };
        let value: serde_json::Value = serde_json::to_value(&call).expect("serialize tool call");
        let obj = value.as_object().expect("tool call is an object");
        assert!(!obj.contains_key("result"));
    }

    #[test]
    fn tool_result_deserialize_missing_success_defaults_true() {
        // success puuttuu → default_success() palauttaa true.
        let json = r#"{"tool_call_id": "call_1", "content": "output"}"#;
        let result: AcpToolResult = serde_json::from_str(json).expect("deserialize tool result");
        assert_eq!(result.tool_call_id, "call_1");
        assert_eq!(result.content, "output");
        assert!(result.success);
    }

    #[test]
    fn tool_result_deserialize_explicit_success_false_is_respected() {
        let json = r#"{"tool_call_id": "call_1", "content": "boom", "success": false}"#;
        let result: AcpToolResult = serde_json::from_str(json).expect("deserialize tool result");
        assert!(!result.success);
    }

    #[test]
    fn token_usage_roundtrips_through_json() {
        let usage = AcpTokenUsage {
            input_tokens: 42,
            output_tokens: 99,
        };
        let json = serde_json::to_string(&usage).expect("serialize usage");
        let decoded: AcpTokenUsage = serde_json::from_str(&json).expect("deserialize usage");
        assert_eq!(decoded.input_tokens, 42);
        assert_eq!(decoded.output_tokens, 99);
    }
}
