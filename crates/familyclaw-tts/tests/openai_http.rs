//! Integration test: the [`OpenAiTts`] `/audio/speech` adapter's HTTP paths
//! (success + error), against a real `reqwest` transport.
//!
//! The mock is just a `std::net::TcpListener` (no `wiremock`/`httpmock`
//! dependency) -- same approach as
//! `familyclaw-channels/tests/telegram_http_errors.rs`. The URL is injected
//! via [`OpenAiTts::with_api_base`] (no source refactoring, no real network
//! access, no API key needed).

#![cfg(feature = "openai")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use familyclaw_tts::{AudioFormat, OpenAiTts, TtsError, TtsProvider, TtsRequest};

/// A minimal HTTP/1.1 mock: accepts a connection, drains the request, and
/// responds with a fixed status + body. Counts received requests.
struct MockOpenAi {
    base_url: String,
    calls: Arc<AtomicUsize>,
}

enum MockBody {
    /// Binary audio payload, `Content-Type: audio/mpeg`, status 200.
    Audio(&'static [u8]),
    /// A JSON error body at the given status (`OpenAI`'s real error shape).
    JsonError(u16, &'static str),
}

impl MockOpenAi {
    fn spawn(body: MockBody) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind to ephemeral port");
        let addr = listener.local_addr().expect("mock local_addr");
        let base_url = format!("http://{addr}");
        let calls = Arc::new(AtomicUsize::new(0));

        let calls_t = Arc::clone(&calls);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                calls_t.fetch_add(1, Ordering::SeqCst);

                let mut buf = [0_u8; 4096];
                let _ = stream.read(&mut buf).unwrap_or(0);

                let response = match &body {
                    MockBody::Audio(bytes) => {
                        let mut head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        )
                        .into_bytes();
                        head.extend_from_slice(bytes);
                        head
                    }
                    MockBody::JsonError(status, reason) => {
                        let json = format!(
                            r#"{{"error":{{"message":"{reason}","type":"invalid_request_error"}}}}"#
                        );
                        format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                            json.len()
                        )
                        .into_bytes()
                    }
                };
                let _ = stream.write_all(&response);
                let _ = stream.flush();
            }
        });

        Self { base_url, calls }
    }

    fn total_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

fn client_pointing_at(base_url: &str) -> OpenAiTts {
    OpenAiTts::with_api_base("test-key", base_url).expect("client builds")
}

#[tokio::test]
async fn synthesize_200_returns_audio_bytes() {
    let mock = MockOpenAi::spawn(MockBody::Audio(b"\x00fake-mp3-bytes\x00"));
    let client = client_pointing_at(&mock.base_url);

    let request = TtsRequest::new("hei maailma")
        .expect("valid")
        .with_format(AudioFormat::Mp3);
    let audio = client
        .synthesize(request)
        .await
        .expect("200 must yield audio, not an error");

    assert_eq!(audio.bytes, b"\x00fake-mp3-bytes\x00");
    assert_eq!(audio.provider, "openai");
    assert_eq!(audio.format, AudioFormat::Mp3);
    assert_eq!(
        mock.total_calls(),
        1,
        "adapter must have hit the server once"
    );
}

#[tokio::test]
async fn synthesize_401_is_backend_error_not_panic() {
    let mock = MockOpenAi::spawn(MockBody::JsonError(401, "Unauthorized"));
    let client = client_pointing_at(&mock.base_url);

    let err = client
        .synthesize(TtsRequest::new("hei").expect("valid"))
        .await
        .expect_err("401 must surface as an error, not Ok");

    assert!(
        matches!(err, TtsError::Backend { status: 401, .. }),
        "expected TtsError::Backend{{status: 401}}, got: {err:?}"
    );
    assert!(
        err.to_string().contains("401"),
        "error must carry the status: {err}"
    );
}

#[tokio::test]
async fn synthesize_500_is_backend_error() {
    let mock = MockOpenAi::spawn(MockBody::JsonError(500, "Internal Server Error"));
    let client = client_pointing_at(&mock.base_url);

    let err = client
        .synthesize(TtsRequest::new("hei").expect("valid"))
        .await
        .expect_err("500 must surface as an error");

    assert!(matches!(err, TtsError::Backend { status: 500, .. }));
}

#[tokio::test]
async fn synthesize_connection_refused_is_request_error_not_panic() {
    // Bind then immediately drop the listener -> port is very likely closed,
    // so the connection attempt fails at the transport level (not an HTTP
    // status). This must surface as TtsError::Request, never a panic.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);

    let client = OpenAiTts::with_api_base("test-key", format!("http://{addr}")).expect("builds");
    let err = client
        .synthesize(TtsRequest::new("hei").expect("valid"))
        .await
        .expect_err("connection refused must surface as an error");

    assert!(
        matches!(err, TtsError::Request { .. }),
        "expected TtsError::Request on connection refused, got: {err:?}"
    );
}
