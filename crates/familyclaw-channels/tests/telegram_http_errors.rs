//! Integraatiotesti: Telegram `sendMessage`-adapterin HTTP-**virhepolut**.
//!
//! Kartoitus paljasti aukon: `send`-onnistumispolku oli katettu (puhtaat
//! parsinta-testit + rakenne), mutta ei-2xx-vastaukset (429 rate-limit, 5xx
//! palvelinvirhe) ja verkkovirhe (yhteys torjuttu / timeout) olivat
//! **testaamattomia**. Tämä tiedosto ajaa oikean `reqwest`-kuljetuksen
//! mock-HTTP-palvelinta vasten ja todistaa että jokainen virhepolku palauttaa
//! selkeän [`ChannelError::Send`]:n — **ei paniikkia, ei valheellista `Ok`:ta**.
//!
//! Mock on pelkkä `std::net::TcpListener` (ei `wiremock`/`httpmock`-dependencyä),
//! joten tämä ei lisää yhtään dev-dependencyä eikä riko `cargo-deny`-gatea —
//! sama linja kuin `familyclaw-agent/tests/live_executor_http.rs`:llä. URL
//! injektoidaan valmiin [`TelegramChannel::with_api_base`]-konstruktorin kautta
//! (ei lähdekoodin refaktorointia).

#![cfg(feature = "telegram")]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use familyclaw_channels::{Channel, ChannelError, OutboundMessage, TelegramChannel};

/// Minimaalinen HTTP/1.1-mock ilman axumia: hyväksyy yhteyden, lukee pyynnön ja
/// vastaa annetulla statuksella + pienellä JSON-rungolla. Laskee saadut pyynnöt,
/// jotta voimme todentaa että adapteri tosiaan otti yhteyden. Palauttaa
/// `base_url`:n jonka `with_api_base` ottaa vastaan.
struct MockTelegram {
    base_url: String,
    calls: Arc<AtomicUsize>,
}

impl MockTelegram {
    /// Käynnistää mockin joka vastaa `status`-koodilla jokaiseen pyyntöön.
    fn spawn(status: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("mock bind to ephemeral port");
        let addr = listener.local_addr().expect("mock local_addr");
        let base_url = format!("http://{addr}");
        let calls = Arc::new(AtomicUsize::new(0));

        let calls_t = Arc::clone(&calls);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                calls_t.fetch_add(1, Ordering::SeqCst);

                // Lue pyyntö (headerit + osa bodya) — sisältöä ei tarvita, vain
                // se että pyyntö kulutetaan ennen vastausta.
                let mut buf = [0_u8; 2048];
                let _ = stream.read(&mut buf).unwrap_or(0);

                let reason = match status {
                    429 => "Too Many Requests",
                    500 => "Internal Server Error",
                    502 => "Bad Gateway",
                    503 => "Service Unavailable",
                    _ => "Error",
                };
                // Telegram-tyylinen virherunko; adapteri ei jäsennä tätä
                // ei-2xx-polulla vaan palauttaa statuksen + rungon virheeseen.
                let body =
                    format!(r#"{{"ok":false,"error_code":{status},"description":"{reason}"}}"#);
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        Self { base_url, calls }
    }

    fn total_calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

/// Rakentaa mockiin osoittavan Telegram-kanavan.
fn channel_pointing_at(base_url: &str) -> TelegramChannel {
    TelegramChannel::with_api_base("test-token", "tg-test", base_url).expect("channel builds")
}

/// Apuri: yksi lähetettävä viesti.
fn outbound() -> OutboundMessage {
    OutboundMessage::new("123456", "hei maailma").expect("valid outbound")
}

#[tokio::test]
async fn send_message_429_rate_limit_is_send_error_not_panic() {
    // 429 Too Many Requests → adapteri EI panikoi, palauttaa ChannelError::Send
    // jonka teksti sisältää statuksen (uudelleenyritettävä virhe).
    let mock = MockTelegram::spawn(429);
    let ch = channel_pointing_at(&mock.base_url);

    let err = ch
        .send(outbound())
        .await
        .expect_err("429 must surface as an error, not Ok");

    assert!(
        matches!(err, ChannelError::Send { .. }),
        "expected ChannelError::Send on 429, got: {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("429"),
        "error should carry the 429 status: {msg}"
    );
    assert_eq!(
        mock.total_calls(),
        1,
        "adapter must have hit the server once"
    );
}

#[tokio::test]
async fn send_message_500_server_error_is_send_error_not_panic() {
    // 5xx palvelinvirhe → ChannelError::Send (ei paniikki, ei valheellinen Ok).
    let mock = MockTelegram::spawn(500);
    let ch = channel_pointing_at(&mock.base_url);

    let err = ch
        .send(outbound())
        .await
        .expect_err("500 must surface as an error");

    assert!(
        matches!(err, ChannelError::Send { .. }),
        "expected ChannelError::Send on 500, got: {err:?}"
    );
    assert!(
        err.to_string().contains("500"),
        "error should carry status 500"
    );
}

#[tokio::test]
async fn send_message_503_service_unavailable_is_send_error() {
    // 503 (yleinen ylikuormatilanne) → ChannelError::Send.
    let mock = MockTelegram::spawn(503);
    let ch = channel_pointing_at(&mock.base_url);

    let err = ch
        .send(outbound())
        .await
        .expect_err("503 must surface as an error");

    assert!(
        matches!(err, ChannelError::Send { .. }),
        "expected ChannelError::Send on 503, got: {err:?}"
    );
}

#[tokio::test]
async fn send_message_network_error_is_send_error_not_panic() {
    // Verkkovirhe: sidotaan portti, luetaan osoite ja SULJETAAN listener heti,
    // jolloin yhteys torjutaan (connection refused). Tämä on deterministinen
    // korvike timeoutille — sama koodipolku (`reqwest::send` -> Err) tuottaa
    // ChannelError::Send:n ilman että testi joutuu odottamaan HTTP-timeouttia.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener); // portti vapautuu → seuraava yhteys torjutaan
    let base_url = format!("http://{addr}");

    let ch = channel_pointing_at(&base_url);

    let err = ch
        .send(outbound())
        .await
        .expect_err("a refused connection must surface as an error, not Ok/panic");

    assert!(
        matches!(err, ChannelError::Send { .. }),
        "expected ChannelError::Send on network failure, got: {err:?}"
    );
}
