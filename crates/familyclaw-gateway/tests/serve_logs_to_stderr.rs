//! Regression test for a "runtime log stays 0 bytes" class of bug: gateway
//! logs must land on **stderr** (not stdout), and must be flushed promptly
//! even for a short-lived process -- not silently swallowed by launcher-
//! dependent stdout buffering. See main.rs's tracing_subscriber::fmt() init
//! (2026-07-25 STATUS note) for the fix this guards.

use std::io::Read;
use std::process::{Command, Stdio};

#[test]
fn serve_startup_logs_go_to_stderr_not_stdout() {
    let exe = env!("CARGO_BIN_EXE_familyclaw-gateway");
    // No channel token configured -> `serve` fails fast (after emitting its
    // startup info!() line), so this test doesn't hang waiting on a server.
    let mut child = Command::new(exe)
        .arg("serve")
        .env("RUST_LOG", "info")
        .env("FAMILYCLAW_GATEWAY_ADDR", "127.0.0.1:0")
        .env_remove("TELEGRAM_BOT_TOKEN")
        .env_remove("FAMILYCLAW_CHANNEL_KIND")
        .env_remove("FAMILYCLAW_GATEWAY_TOKEN")
        .env_remove("FAMILYCLAW_CONFIG")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn familyclaw-gateway");

    let status = child.wait().expect("wait for child");
    assert!(
        !status.success(),
        "expected `serve` to fail fast without a configured channel token"
    );

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout pipe")
        .read_to_string(&mut stdout)
        .expect("read stdout");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr pipe")
        .read_to_string(&mut stderr)
        .expect("read stderr");

    assert!(
        stderr.contains("familyclaw-gateway"),
        "expected the startup tracing line on stderr, got: {stderr:?}"
    );
    assert!(
        stdout.is_empty(),
        "expected no tracing output on stdout, got: {stdout:?}"
    );
}
