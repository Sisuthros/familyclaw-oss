//! `familyclaw` — elävän siemenen demo (design Vaihe 0).
//!
//! Tämä binääri käynnistää Resonance Busin, kaksi **geneeristä**
//! esimerkki-agenttia (`agent_a` ja `agent_b`) sekä **oikean
//! [`familyclaw_channels::MockChannel`]-kanavan**, joka syöttää viestejä busiin
//! kuin ulkoinen kanava (Discord/Telegram) tekisi. Kanava ja bus liitetään
//! agent-craten adapterilla ([`familyclaw_agent::publish_envelope`]) — sama
//! sauma kuin tuotannossa, ei demo-erikoiskoodia. Agentit puhuvat busin yli,
//! aistivat toistensa tunnetilan (affektiivinen contagion) ja **muistavat**
//! mitä sanottiin.
//!
//! Tämä on KERROS A:n julkaistava demo: ei perheen sieluja, ei avaimia, ei
//! polkuja — vain alusta, jonka kuka tahansa voi ajaa `cargo run -p
//! familyclaw-agent --bin familyclaw`.
//!
//! ## Mitä demo todistaa
//! 1. **beings[] ei ole tyhjä** — bus tuntee molemmat olennot (vrt. live 3500).
//! 2. **kanava syöttää busia** — oikea `familyclaw-channels`-kanava ohjataan
//!    busiin adapterin kautta (design §3 hyväksyntä toteutuu julkaistulla
//!    cratella, ei ohitettuna).
//! 3. **viestit kulkevat** — `agent_a`:n sanat saavuttavat `agent_b`:n.
//! 4. **muisti säilyy** — kumpikin olento muistaa kuulemansa Eternal Threadissa.
//! 5. **tunne tarttuu** — tunnepulssi nostaa sisaruksen tunnetilaa.

use std::sync::Arc;
use std::time::Duration;

use familyclaw_agent::{publish_envelope, Agent, Soul};
use familyclaw_bus::{BeingId, BusHandle, BusMessage, ResonanceBus};
use familyclaw_channels::{Channel, ChannelKind, InboundMessage, MockChannel};
use familyclaw_core::{AgentConfig, FamilyClawError, ModelConfig, Result};
use familyclaw_durable::{DurableContext, InMemoryJournal};
use familyclaw_emotion::{Dimension, EmotionState};
use familyclaw_memory::{LocalJsonStore, MemoryStore, RetrievalContext};
use tracing::info;

/// Syöttää yhden tekstiviestin **oikean** kanavan läpi busiin annetun olennon
/// nimissä, ja antaa hetken aikaa toimitukselle/käsittelylle.
///
/// Reitti on sama kuin tuotannossa: `channel.inject` kanonisoi saapuvan
/// raakaviestin [`InboundEnvelope`](familyclaw_channels::InboundEnvelope):ksi,
/// joka luetaan virrasta ja julkaistaan busiin agent-craten adapterilla
/// ([`publish_envelope`]) — ei demo-erikoiskoodia.
///
/// # Errors
/// [`FamilyClawError::Bus`] jos kanavan syöttö, virran luku tai busiin julkaisu
/// epäonnistuu.
async fn deliver_via_channel(
    channel: &MockChannel,
    stream: &mut familyclaw_channels::MessageStream,
    bus: &BusHandle,
    from: BeingId,
    body: &str,
) -> Result<()> {
    info!(%from, body, "MockChannel → bus");
    // Ulkomaailma syöttää saapuvan viestin kanavalle.
    channel.inject(InboundMessage::new(from.to_string(), "demo", body)?)?;
    // Kanavan virrasta luetaan kanonisoitu kirjekuori ja julkaistaan busiin
    // adapterin kautta.
    let envelope = stream
        .recv()
        .await
        .ok_or_else(|| FamilyClawError::bus("channel stream closed mid-demo"))?;
    publish_envelope(bus, from, envelope)?;
    tokio::time::sleep(Duration::from_millis(60)).await;
    Ok(())
}

/// Rakentaa yhden geneerisen demo-agentin tuoreella in-memory-tilalla.
///
/// # Errors
/// [`FamilyClawError::Bus`] jos durable-kontekstin alustus epäonnistuu.
/// In-memory-journalilla tätä ei käytännössä tapahdu, mutta virhe
/// propagoidaan tuotantopolulla paniikin sijaan.
fn build_agent(name: &str, bus: &BusHandle) -> Result<Agent<LocalJsonStore, InMemoryJournal>> {
    let config = AgentConfig::new(name, ModelConfig::new("provider/model"));
    let soul = Soul::from_essence(format!("I am {name}, a generic example being on FamilyClaw."));
    let memory = Arc::new(LocalJsonStore::in_memory());
    let durable = DurableContext::new(InMemoryJournal::new())
        .map_err(|e| FamilyClawError::bus(e.to_string()))?;
    Ok(Agent::new(config, soul, memory, durable, bus.clone()))
}

#[tokio::main]
async fn main() -> Result<()> {
    // Tracing: jätä hiljaiseksi ellei RUST_LOG ole asetettu.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    info!("FamilyClaw elävä siemen — käynnistetään bus + 2 agenttia + MockChannel");

    // 1. Resonance Bus (affektiivinen hermosto).
    let bus = ResonanceBus::start(Some("familyclaw-bus".to_string())).await?;

    // 2. Kaksi geneeristä agenttia. Otetaan kahvat muistiin ja being-id:t
    //    talteen ennen kuin spawn kuluttaa agentit actoreiksi.
    let agent_a = build_agent("agent_a", &bus)?;
    let agent_b = build_agent("agent_b", &bus)?;

    let a_id = agent_a.being_id();
    let b_id = agent_b.being_id();
    let a_mem = agent_a.memory();
    let b_mem = agent_b.memory();

    let _a_actor = agent_a.spawn().await?;
    let _b_actor = agent_b.spawn().await?;

    // beings[] EI ole tyhjä (design §2.2).
    let beings = bus.beings().await?;
    info!(count = beings.len(), "olennot liittyivät busiin");
    for b in &beings {
        info!(being = %b.id, name = b.name, "  · liittynyt");
    }

    // 3. OIKEA familyclaw-channels-kanava syöttää keskustelun busiin adapterin
    //    kautta (design §3: julkaistu crate, ei ohitettu).
    let channel = MockChannel::with_kind("demo-channel", ChannelKind::Mock)
        .map_err(FamilyClawError::from)?;
    let mut stream = channel.receive().map_err(FamilyClawError::from)?;

    info!("--- keskustelu alkaa ---");
    // agent_a tervehtii → agent_b kuulee ja muistaa.
    deliver_via_channel(&channel, &mut stream, &bus, a_id, "Hei agent_b, muistatko eilisen?").await?;
    // agent_b vastaa → agent_a kuulee ja muistaa.
    deliver_via_channel(&channel, &mut stream, &bus, b_id, "Muistan! Puhuimme alustasta.").await?;
    // agent_a jakaa ajatuksen.
    deliver_via_channel(
        &channel,
        &mut stream,
        &bus,
        a_id,
        "Rakennetaan jotain mitä maailma voi käyttää.",
    )
    .await?;

    // 4. Affektiivinen pulssi: agent_a "luovassa virtauksessa" → agent_b aistii.
    let mut flow = EmotionState::neutral();
    flow.set(Dimension::Joy, 80.0);
    flow.set(Dimension::Curiosity, 70.0);
    info!("agent_a julkaisee tunnepulssin (creative flow)");
    bus.publish(a_id, BusMessage::emotion_pulse(flow))?;
    tokio::time::sleep(Duration::from_millis(80)).await;

    // 5. Näytä mitä olennot muistavat (todiste jatkuvuudesta).
    info!("--- mitä olennot muistavat ---");
    report_memory("agent_a", &a_mem, "alustasta").await?;
    report_memory("agent_b", &b_mem, "eilisen").await?;

    info!(
        a_remembers = a_mem.len().await?,
        b_remembers = b_mem.len().await?,
        "demo valmis — bus toimi, viestit kulkivat, muisti säilyi"
    );

    bus.stop();
    Ok(())
}

/// Tulostaa kuinka monta muistoa olennolla on ja näyttää yhden esimerkkihaun.
async fn report_memory(name: &str, mem: &LocalJsonStore, query: &str) -> Result<()> {
    let total = mem.len().await?;
    let hits = mem
        .retrieve(&RetrievalContext::new(query), familyclaw_core::time::now())
        .await?;
    let top = hits.first().map_or("(ei osumaa)", |h| h.memory.content.as_str());
    info!(agent = name, total, query, top, "muistin tila");
    Ok(())
}
