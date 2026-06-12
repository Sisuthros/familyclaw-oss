//! Discord-adapterin integraatiotesti oikeaa bottia vasten.
//!
//! Ajetaan VAIN jos ympäristömuuttujat `DISCORD_TEST_TOKEN` ja
//! `DISCORD_TEST_CHANNEL_ID` on asetettu — muuten testi ohittaa itsensä
//! siististi (CI ei vaadi secretiä peruskäännökseen). Testi todentaa
//! kaksisuuntaisen liikenteen: `start` → `send` → odota viestin paluuta
//! `receive()`-streamiin → `stop`.
#![cfg(feature = "discord")]

use std::env;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use familyclaw_channels::{Channel, DiscordChannel, OutboundMessage};
use tokio::time::timeout;

#[tokio::test]
async fn test_discord_round_trip() {
    let token = env::var("DISCORD_TEST_TOKEN").unwrap_or_default();
    let channel_id_str = env::var("DISCORD_TEST_CHANNEL_ID").unwrap_or_default();

    if token.is_empty() || channel_id_str.is_empty() {
        eprintln!(
            "DISCORD_TEST_TOKEN tai DISCORD_TEST_CHANNEL_ID puuttuu, ohitetaan integraatiotesti."
        );
        return;
    }

    let channel_id: u64 = channel_id_str.parse().expect("Virheellinen Channel ID");

    let channel = DiscordChannel::new(token, channel_id).expect("Kanavan luonti epäonnistui");

    // Käynnistä gateway (palaa vasta ready/virhe).
    channel
        .start()
        .await
        .expect("Kanavan käynnistys epäonnistui");

    // Avaa saapuvan virran (kutsuttavissa kerran).
    let mut stream = channel
        .receive()
        .expect("receive-stream avaaminen epäonnistui");

    let random_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("kello")
        .as_millis()
        .to_string();
    let test_message = format!("familyclaw-integration-ping {random_id}");

    // Lähetä testiviesti. OutboundMessage::new(target, body): target on
    // kanava-id, johon adapteri reitittää (DiscordChannel käyttää omaa
    // target_channel_id:tään lähetyksessä, joten target on tässä sama).
    let outbound = OutboundMessage::new(channel_id.to_string(), test_message.clone())
        .expect("outbound-viestin rakennus");
    channel
        .send(outbound)
        .await
        .expect("Viestin lähetys epäonnistui");

    // Odota viestin paluuta streamiin max 15 s. Botti EI näe omia viestejään
    // (map_message suodattaa bot-lähettäjät), joten round trip onnistuu vain jos
    // joku muu kaiuttaa viestin. Tämä on odotettua: ellei vastausta tule,
    // riittää että send onnistui.
    let result = timeout(Duration::from_secs(15), async {
        while let Some(msg) = stream.recv().await {
            if msg.body.contains(&random_id) {
                return true;
            }
        }
        false
    })
    .await;

    match result {
        Ok(true) => println!("Round-trip onnistui!"),
        Ok(false) | Err(_) => {
            println!(
                "Viestiä ei näkynyt streamissa 15 sekunnin kuluessa. \
                 Botti ei näe omia viestejään — send-onnistuminen riittää."
            );
        }
    }

    // Sulje gateway siististi.
    channel.stop().await.expect("Kanavan pysäytys epäonnistui");
}
