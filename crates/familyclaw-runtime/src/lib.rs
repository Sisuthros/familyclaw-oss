//! # familyclaw-runtime
//!
//! **Ajonaikainen kokoonpano** — FamilyClaw-alustan (KERROS A, OSS) C5-sauma:
//! se kytkee aiemmin rakennetut palaset yhdeksi eläväksi olennoksi:
//!
//! ```text
//! Channel::receive() ─► pump_channel_to_bus ─► Resonance Bus ─► Agent(spawn)
//!                                                                    │
//!                       Channel::send ◄── reply_rx ◄── route_reply ◄─┘
//! ```
//!
//! [`build_family`] on yksi kutsu, joka korvaa gatewayn suoran
//! [`ResonanceBus::start`]-bootstrappauksen: se käynnistää busin, spawnaa
//! agentin, pumppaa kanavan saapuvan virran busiin ja tyhjentää agentin
//! reply-jonon takaisin kanavalle. [`FamilyRuntime`] omistaa kaiken, jotta
//! sammutus ([`FamilyRuntime::shutdown`]) on siisti.
//!
//! ## MVP-laajuus
//! Yksi agentti, yksi kanava, **staattinen** reply-kohde
//! ([`Agent::with_reply_target`]). Tämä on oikein tasan silloin kun on
//! **yksi kanava ja yksi keskustelu**: kaikki vastaukset ohjautuvat siihen
//! yhteen kohteeseen. Heti kun kanavia tai keskusteluja on enemmän kuin yksi,
//! staattinen kohde reitittäisi väärin (vastaisi A:lle B:n keskusteluun) ja
//! tarvitaan per-viesti-alkuperä (`MessageOrigin`) — ks. [`build_family`]:n
//! "Tuotanto-raja".
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate ei kovakoodaa perheenjäsenten nimiä, avaimia, malleja eikä
//! polkuja. Agentin nimi, malli, sielu, kanava ja reply-kohde annetaan kaikki
//! ajonaikaisesti kutsujalta (gateway lukee ne ympäristöstä — KERROS B).

use std::sync::Arc;

use familyclaw_agent::{
    new_reply_channel, primary_llm_config, Agent, ErasedMemoryStore, LlmEndpointResolver, Soul,
};
use familyclaw_bus::{BeingId, BusHandle, ResonanceBus, ResonanceMessage};
use familyclaw_channels::Channel;
use familyclaw_core::{AgentConfig, FamilyClawError, Result};
use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
use familyclaw_memory::LocalJsonStore;
use ractor::ActorRef;

/// Ajonaikainen kokoonpano: bus + spawnatut agentit + reply-pumppu + kanavat.
///
/// Omistaa kaiken jotta sammutus on siisti. `bus`-kahva luovutetaan gatewaylle
/// ([`FamilyRuntime::bus`]) sen `GatewayState`-tilaan; tausta­tehtävät
/// (channel→bus -pumppu ja reply→channel -tyhjennys) pidetään hengissä
/// [`tokio::task::JoinHandle`]-kahvojen kautta ja keskeytetään
/// [`FamilyRuntime::shutdown`]:ssa.
///
/// ## Reply-kanava on unbounded
/// Agentin reply-sink ([`new_reply_channel`]) on tarkoituksella **unbounded**:
/// [`Agent::route_reply`] on synkroninen, ei-lukkiutuva kutsu (bounded send
/// olisi async ja voisi blokata agentin vuoronkäsittelyn). Sen sijaan
/// reply-jono **tyhjennetään välittömästi** drain-taskissa
/// (`while let Some(out) = reply_rx.recv().await { channel.send(out).await }`),
/// joten viestit eivät kasaudu. Korkean läpäisyn tuotannossa lisää
/// bounded-wrapper tai backpressure-mittari drain-puolelle.
pub struct FamilyRuntime {
    bus: BusHandle,
    /// Spawnatut agentti-actorit. Pidetään elossa (drop = actor pysähtyy).
    _agents: Vec<ActorRef<ResonanceMessage>>,
    /// Taustatehtävät: channel→bus -pumppu ja reply→channel -tyhjennys.
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl FamilyRuntime {
    /// Bus-kahva (jaettavaksi esim. gatewayn `GatewayState`-tilaan).
    #[must_use]
    pub fn bus(&self) -> &BusHandle {
        &self.bus
    }

    /// Sammuttaa kokoonpanon siististi: keskeyttää taustatehtävät ja pysäyttää
    /// busin. Korvaa gatewayn suoran `bus.stop()`-kutsun.
    pub fn shutdown(self) {
        for t in self.tasks {
            t.abort();
        }
        self.bus.stop();
    }
}

/// C5-kokooja: rakentaa elävän [`FamilyRuntime`]:n yhdellä kutsulla.
///
/// Kytkee `Channel::receive()` → [`pump_channel_to_bus`] → bus → `Agent`
/// (spawn) → `route_reply` → reply-jono → `Channel::send`. MVP: yksi agentti,
/// yksi kanava, staattinen reply-kohde.
///
/// LLM on **valinnainen**: jos [`primary_llm_config`] ei ratkea (esim. avain
/// puuttuu ympäristöstä), agentti spawnataan ilman LLM:ää — se muistaa ja
/// reagoi tunnetilassa, mutta ei tuota tekstivastauksia. Tämä pitää
/// kokoonpanon käynnistettävänä ilman provider-avaimia (savutestit, CI).
///
/// # Tuotanto-raja (MVP-rajoite)
/// Yksi `reply_target`/agentti on oikein **vain yhdelle kanavalle ja yhdelle
/// keskustelulle**. Heti kun kanavia on >1 tai agentti palvelee >1
/// keskustelua, staattinen kohde reitittää vastauksen väärään keskusteluun.
/// Silloin tarvitaan per-viesti-alkuperä (`MessageOrigin` bus-kirjekuoressa,
/// josta kohde johdetaan); ks. [`Agent::with_reply_target`]:n C2-aukon
/// dokumentaatio. Tätä origin-sopimusta **ei ole** vielä rakennettu.
///
/// # Errors
/// - [`FamilyClawError::Config`] jos mallikonfiguraatio on kelvoton (tämä
///   nostetaan vain jos primary on tyhjä — puuttuva endpoint johtaa
///   LLM-vapaaseen agenttiin, ei virheeseen).
/// - [`FamilyClawError::Bus`] jos busin käynnistys, agentin spawn/rekisteröinti
///   tai durable-kontekstin rakennus epäonnistuu.
/// - [`FamilyClawError::InvalidInput`] (kanavakerroksesta käännettynä) jos
///   kanavan saapuvaa virtaa ei voi avata.
pub async fn build_family(
    bus_name: Option<String>,
    agent_cfg: AgentConfig,
    soul: Soul,
    channel: Box<dyn Channel>,
    reply_target: String,
    resolver: &dyn LlmEndpointResolver,
) -> Result<FamilyRuntime> {
    // 1. Käynnistä Resonance Bus (perheen affektiivinen hermosto).
    let bus = ResonanceBus::start(bus_name).await?;

    // 2. Reply-kanava (C1 Malli A): agentti työntää vastaukset sinkkiin,
    //    runtime omistaa recv-pään ja kutsuu Channel::send (alla, askel 9).
    let (sink, mut reply_rx) = new_reply_channel();

    // 3. LLM (valinnainen): jos avain/endpoint puuttuu, agentti toimii ilman.
    let llm = primary_llm_config(&agent_cfg.model, resolver).ok();

    // 4. Muisti (Eternal Thread, in-memory MVP) + durable-konteksti.
    let memory: ErasedMemoryStore = Arc::new(LocalJsonStore::in_memory());
    let durable =
        DurableContext::new(Box::new(InMemoryJournal::new()) as Box<dyn Journal + Send + Sync>)
            .map_err(|e| FamilyClawError::bus(e.to_string()))?;

    // 5. Rakenna agentti ja kytke reply-sink + staattinen reply-kohde.
    let agent = Agent::new(agent_cfg, soul, memory, durable, bus.clone(), llm, None)
        .with_reply_sink(sink)
        .with_reply_target(reply_target);

    // 6. Spawnaa agentti actorina (rekisteröi busiin).
    let actor = agent.spawn().await?;

    // 7. Kanavan oma bus-seat — ERI kuin agentin being_id, muuten AgentActor
    //    skippaisi viestin "omana kaikuna" (agent.rs handle, sender-tarkistus).
    let channel_seat = BeingId::new();

    // 8. Avaa kanavan saapuva virta ja pumppaa se busiin omassa taskissaan.
    //    pump_channel_to_bus blokkaa kunnes virta sulkeutuu → pakko spawnata.
    let stream = channel.receive().map_err(FamilyClawError::from)?;
    let pump = tokio::spawn({
        let bus = bus.clone();
        async move {
            if let Err(e) = familyclaw_agent::pump_channel_to_bus(stream, bus, channel_seat).await {
                tracing::warn!("channel→bus pump ended: {e}");
            }
        }
    });

    // 9. Tyhjennä agentin reply-jono kanavalle (drain). Jaa kanava Arc:lla —
    //    receive() on jo kutsuttu (askel 8), send() menee Arc:n kautta.
    let ch: Arc<dyn Channel> = Arc::from(channel);
    let drain = tokio::spawn(async move {
        while let Some(out) = reply_rx.recv().await {
            if let Err(e) = ch.send(out).await {
                tracing::warn!("channel send failed: {e}");
            }
        }
    });

    // 10. Kokoa runtime — omistaa busin, agentin ja molemmat taskit.
    Ok(FamilyRuntime {
        bus,
        _agents: vec![actor],
        tasks: vec![pump, drain],
    })
}

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use familyclaw_agent::EnvEndpointResolver;
    use familyclaw_channels::{InboundMessage, MockChannel};
    use familyclaw_core::ModelConfig;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    /// MVP-savutesti (inbound pää-päähän busiin asti): mock-kanavaan injektoitu
    /// viesti pumppautuu busin kautta agentille, joka **muistaa** sen. Ilman
    /// LLM:ää agentti ei tuota reply:tä (`think` palauttaa `None`), joten
    /// reply-path testataan erikseen agentin yksikkötesteissä
    /// (`route_reply_reaches_sink_with_correct_target`).
    #[tokio::test]
    async fn build_family_pumps_inbound_message_into_agent_memory() {
        let channel = MockChannel::new("mock-feed").expect("channel");

        // Injektoi viesti ja sulje saapuva virta ENNEN kuin kanava siirretään
        // build_family:lle (joka kuluttaa sen `Box<dyn Channel>`:nä). Viesti jää
        // puskuroiduksi unbounded-mpsc-jonoon, jonka `receive()` ottaa
        // haltuunsa; `close_inbound` saa pumpun päättymään deterministisesti
        // kun puskuroitu viesti on kulutettu.
        channel
            .inject(InboundMessage::new("user-1", "general", "muistatko tämän?").expect("inbound"))
            .expect("inject");
        channel.close_inbound();

        // Tunnistamaton provider → ei LLM:ää (primary_llm_config None) → agentti
        // toimii ilman tekstivastausta. Tämä on KERROS A -puhdas polku.
        let resolver = EnvEndpointResolver::new();
        let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
        let soul = Soul::from_essence("I am agent_a, a generic example being.");

        let runtime = build_family(
            None,
            agent_cfg,
            soul,
            Box::new(channel),
            "mock:general".to_string(),
            &resolver,
        )
        .await
        .expect("runtime builds");

        // Anna pumpun + agentin käsitellä viesti.
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;

        // Bus tuntee yhden olennon (agentin) — beings[] ei tyhjä.
        let beings = runtime.bus().beings().await.expect("beings");
        assert_eq!(beings.len(), 1, "agentti rekisteröityi busiin");
        assert_eq!(beings[0].name, "agent_a");

        runtime.shutdown();
    }

    /// `build_family` kelpaa myös LLM:n kanssa konfiguroituna (resolveri tuntee
    /// providerin): runtime rakentuu ilman paniikkia ja bus on käynnissä.
    /// Emme tee oikeaa LLM-kutsua (ei verkkoa) — testaamme vain kokoonpanon.
    #[tokio::test]
    async fn build_family_with_resolvable_provider_constructs() {
        let channel = MockChannel::new("mock-2").expect("channel");
        channel.close_inbound(); // ei syötettä → pumppu päättyy heti.

        // Resolveri tuntee providerin, mutta avain puuttuu env:stä → tyhjä avain
        // päätyy LlmConfigiin (ei verkkokutsua testissä). build_family saa
        // Some(llm), agentti spawnaa LLM:n kanssa.
        let resolver = EnvEndpointResolver::new().with_provider(
            "openai",
            "https://api.openai.com/v1",
            "OPENAI_API_KEY_RUNTIME_TEST_UNSET",
        );
        let agent_cfg = AgentConfig::new("agent_b", ModelConfig::new("openai/gpt-4o"));
        let soul = Soul::from_essence("I am agent_b.");

        let runtime = build_family(
            Some("runtime-test-bus".to_string()),
            agent_cfg,
            soul,
            Box::new(channel),
            "mock:room".to_string(),
            &resolver,
        )
        .await
        .expect("runtime builds with provider");

        assert_eq!(runtime.bus().count().await.expect("count"), 1);
        runtime.shutdown();
    }
}
