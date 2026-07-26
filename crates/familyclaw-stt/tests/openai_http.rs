//! Integration test: the [`OpenAiWhisper`] `/audio/transcriptions` adapter's
//! HTTP paths (success + error), against a real `reqwest` transport.
//!
//! The mock is just a `std::net::TcpListener` (no `wiremock`/`httpmock`
//! dependency) -- same approach as
//! `familyclaw-tts/tests/openai_http.rs` and
//! `familyclaw-channels/tests/telegram_http_errors.rs`. The URL is injected
//! via [`OpenAiWhisper::with_api_base`] (no source refactoring, no real
//! network access, no API key needed).

#![cfg(feature = "openai")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use familyclaw_stt::{AudioFormat, OpenAiWhisper, SttError, SttProvider, SttRequest};

/// A minimal HTTP/1.1 mock: accepts a connection, drains the request, and
/// responds with a fixed status + body. Counts received requests.
struct MockOpenAi {
    base_url: String,
    calls: Arc<AtomicUsize>,
}

/// Reads a full HTTP/1.1 request (headers + `Content-Length` body) off
/// `stream`, looping until everything has arrived.
///
/// A single `stream.read()` call (as the TTS/Telegram mocks use for their
/// small, single-segment JSON bodies) is not enough here: a multipart file
/// upload is large enough, and `reqwest` writes headers and the streamed
/// body in separate calls, that the request can arrive across more than
/// one TCP read on localhost. Responding and closing the socket after only
/// a partial read races the client still writing the rest of the body,
/// which the OS then answers with a reset instead of a clean response --
/// this fully drains the declared `Content-Length` first so the response
/// is only sent once the whole request has been received.
fn read_full_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    let mut data = Vec::new();
    let mut buf = [0_u8; 8192];

    let header_end = loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return data,
            Ok(n) => {
                data.extend_from_slice(&buf[..n]);
                if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            }
        }
    };

    let content_length = String::from_utf8_lossy(&data[..header_end])
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);

    let mut remaining = content_length.saturating_sub(data.len() - header_end);
    while remaining > 0 {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                data.extend_from_slice(&buf[..n]);
                remaining = remaining.saturating_sub(n);
            }
        }
    }

    data
}

enum MockBody {
    /// `verbose_json` transcription success, status 200.
    Transcript {
        text: &'static str,
        language: Option<&'static str>,
    },
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

                let _ = read_full_http_request(&mut stream);

                let response = match &body {
                    MockBody::Transcript { text, language } => {
                        let json = match language {
                            Some(lang) => format!(r#"{{"text":"{text}","language":"{lang}"}}"#),
                            None => format!(r#"{{"text":"{text}"}}"#),
                        };
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                            json.len()
                        )
                        .into_bytes()
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

fn client_pointing_at(base_url: &str) -> OpenAiWhisper {
    OpenAiWhisper::with_api_base("test-key", base_url).expect("client builds")
}

#[tokio::test]
async fn transcribe_200_returns_text_and_language() {
    let mock = MockOpenAi::spawn(MockBody::Transcript {
        text: "hei maailma",
        language: Some("finnish"),
    });
    let client = client_pointing_at(&mock.base_url);

    let request =
        SttRequest::new(b"fake-mp3-bytes".to_vec(), AudioFormat::Mp3).expect("valid request");
    let transcript = client
        .transcribe(request)
        .await
        .expect("200 must yield a transcript, not an error");

    assert_eq!(transcript.text, "hei maailma");
    assert_eq!(transcript.language.as_deref(), Some("finnish"));
    assert_eq!(transcript.provider, "openai");
    assert_eq!(
        mock.total_calls(),
        1,
        "adapter must have hit the server once"
    );
}

#[tokio::test]
async fn transcribe_200_without_language_falls_back_to_request_hint() {
    let mock = MockOpenAi::spawn(MockBody::Transcript {
        text: "hello world",
        language: None,
    });
    let client = client_pointing_at(&mock.base_url);

    let request = SttRequest::new(b"fake-wav-bytes".to_vec(), AudioFormat::Wav)
        .expect("valid request")
        .with_language("en");
    let transcript = client
        .transcribe(request)
        .await
        .expect("200 must yield a transcript");

    assert_eq!(transcript.text, "hello world");
    assert_eq!(transcript.language.as_deref(), Some("en"));
}

#[tokio::test]
async fn transcribe_401_is_backend_error_not_panic() {
    let mock = MockOpenAi::spawn(MockBody::JsonError(401, "Unauthorized"));
    let client = client_pointing_at(&mock.base_url);

    let err = client
        .transcribe(SttRequest::new(b"hei".to_vec(), AudioFormat::Mp3).expect("valid"))
        .await
        .expect_err("401 must surface as an error, not Ok");

    assert!(
        matches!(err, SttError::Backend { status: 401, .. }),
        "expected SttError::Backend{{status: 401}}, got: {err:?}"
    );
    assert!(
        err.to_string().contains("401"),
        "error must carry the status: {err}"
    );
}

#[tokio::test]
async fn transcribe_500_is_backend_error() {
    let mock = MockOpenAi::spawn(MockBody::JsonError(500, "Internal Server Error"));
    let client = client_pointing_at(&mock.base_url);

    let err = client
        .transcribe(SttRequest::new(b"hei".to_vec(), AudioFormat::Mp3).expect("valid"))
        .await
        .expect_err("500 must surface as an error");

    assert!(matches!(err, SttError::Backend { status: 500, .. }));
}

#[tokio::test]
async fn transcribe_connection_refused_is_request_error_not_panic() {
    // Bind then immediately drop the listener -> port is very likely closed,
    // so the connection attempt fails at the transport level (not an HTTP
    // status). This must surface as SttError::Request, never a panic.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);

    let client =
        OpenAiWhisper::with_api_base("test-key", format!("http://{addr}")).expect("builds");
    let err = client
        .transcribe(SttRequest::new(b"hei".to_vec(), AudioFormat::Mp3).expect("valid"))
        .await
        .expect_err("connection refused must surface as an error");

    assert!(
        matches!(err, SttError::Request { .. }),
        "expected SttError::Request on connection refused, got: {err:?}"
    );
}
