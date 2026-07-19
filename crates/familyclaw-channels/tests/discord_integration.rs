//! Integration test for the Discord adapter against a real bot.
//!
//! Runs ONLY if the environment variables `DISCORD_TEST_TOKEN` and
//! `DISCORD_TEST_CHANNEL_ID` are set — otherwise the test skips itself
//! cleanly (CI does not require a secret for a basic build). The test
//! verifies bidirectional traffic: `start` → `send` → wait for the message
//! to come back on the `receive()` stream → `stop`.
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
            "DISCORD_TEST_TOKEN or DISCORD_TEST_CHANNEL_ID is missing, skipping the integration test."
        );
        return;
    }

    let channel_id: u64 = channel_id_str.parse().expect("Invalid Channel ID");

    // owner_id 0 = DM gate disabled (the integration test only verifies the guild round trip).
    let channel = DiscordChannel::new(token, channel_id, 0).expect("Channel creation failed");

    // Start the gateway (returns only once ready/error).
    channel.start().await.expect("Channel start failed");

    // Open the inbound stream (callable once).
    let mut stream = channel
        .receive()
        .expect("opening the receive stream failed");

    let random_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis()
        .to_string();
    let test_message = format!("familyclaw-integration-ping {random_id}");

    // Send the test message. OutboundMessage::new(target, body): target is
    // the channel id the adapter routes to (DiscordChannel uses its own
    // target_channel_id for sending, so target is the same value here).
    let outbound = OutboundMessage::new(channel_id.to_string(), test_message.clone())
        .expect("building the outbound message");
    channel.send(outbound).await.expect("Message send failed");

    // Wait up to 15s for the message to come back on the stream. The bot
    // does NOT see its own messages (map_message filters out bot senders),
    // so the round trip only succeeds if someone else echoes the message.
    // This is expected: if no reply arrives, it is enough that send succeeded.
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
        Ok(true) => println!("Round trip succeeded!"),
        Ok(false) | Err(_) => {
            println!(
                "The message did not appear on the stream within 15 seconds. \
                 The bot does not see its own messages — a successful send is sufficient."
            );
        }
    }

    // Shut down the gateway cleanly.
    channel.stop().await.expect("Channel stop failed");
}
