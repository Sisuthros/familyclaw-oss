//! Mock MCP stdio server for integration tests (Layer A).
//!
//! Reads JSON-RPC lines from stdin and responds to the `initialize`,
//! `tools/list`, and `tools/call` methods.

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

fn main() {
    let stdin = io::stdin();
    for line in stdin.lock().lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            continue;
        };

        match method {
            "initialize" => {
                if let Some(id) = msg.get("id") {
                    respond(
                        id,
                        &json!({
                            "protocolVersion": "2024-11-05",
                            "capabilities": {},
                            "serverInfo": { "name": "mock-mcp-stdio-server", "version": "1.0.0" }
                        }),
                    );
                }
            }
            "notifications/initialized" => {}
            "tools/list" => {
                if let Some(id) = msg.get("id") {
                    respond(
                        id,
                        &json!({
                            "tools": [{
                                "name": "echo",
                                "description": "Echoes JSON input back as text.",
                                "inputSchema": { "type": "object" }
                            }]
                        }),
                    );
                }
            }
            "tools/call" => {
                if let Some(id) = msg.get("id") {
                    let name = msg
                        .pointer("/params/name")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let args = msg
                        .pointer("/params/arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    if name == "echo" {
                        let text = args.to_string();
                        respond(
                            id,
                            &json!({
                                "content": [{ "type": "text", "text": text }],
                                "isError": false
                            }),
                        );
                    } else {
                        respond_error(id, &format!("unknown tool: {name}"));
                    }
                }
            }
            _ => {
                if let Some(id) = msg.get("id") {
                    respond_error(id, &format!("unsupported method: {method}"));
                }
            }
        }
    }
}

fn respond(id: &Value, result: &Value) {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    emit(&msg);
}

fn respond_error(id: &Value, message: &str) {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32000, "message": message },
    });
    emit(&msg);
}

fn emit(msg: &Value) {
    let line = msg.to_string();
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}
