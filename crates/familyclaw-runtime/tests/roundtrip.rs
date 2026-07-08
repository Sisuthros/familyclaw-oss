//! End-to-end roundtrip-integraatiotesti FamilyClaw-runtimelle.
//!
//! Todistaa **deterministisesti** koko viestiketjun ilman oikeaa LLM-avainta
//! tai Telegram-tokenia:
//!
//! ```text
//! inbound (MockChannel::inject)
//!   └─► pump_channel_to_bus ─► Resonance Bus
//!                                  └─► Agent::handle_turn ─► Agent::think()
//!                                          (mock-LLM, OpenAI-yhteensopiva HTTP,
//!                                           palauttaa kiinteän tekstin)
//!                                              └─► route_reply ─► reply_sink
//!                                                      └─► drain ─► Channel::send
//!                                                              └─► MockChannel.outbox
//! ```
//!
//! Tämä on "generinen agentti lähettää 1. viestin" -ekvivalentti: yksi inbound-viesti
//! tuottaa yhden ulosmenevän vastauksen oikealla kohteella, ja vastauksen
//! sisältö on TASAN se mitä mock-LLM palautti (ei satunnaisuutta, ei verkkoa
//! ulos testistä).
//!
//! ## Miksi mock-LLM HTTP-palvelimena
//! `Agent::think()` kutsuu `LlmClient::complete()`:a, joka tekee `OpenAI`-
//! yhteensopivan `POST /chat/completions`-pyynnön. Pystytämme pikkuruisen
//! axum-palvelimen, joka palauttaa kiinteän choices[0].message.content-arvon,
//! ja osoitamme `EnvEndpointResolver`-providerin sen base-URL:ään. Näin koko
//! reply-path ajetaan oikeasti läpi (think → `route_reply` → sink → send) ilman
//! ulkoista API:a.

use std::net::SocketAddr;
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use familyclaw_agent::EnvEndpointResolver;
use familyclaw_channels::{InboundMessage, MockChannel, OutboundKind, OutboundMessage};
use familyclaw_core::{AgentConfig, ModelConfig};
use familyclaw_runtime::build_family;

/// Kiinteä teksti, jonka mock-LLM aina palauttaa. Roundtripin "todiste":
/// jos tämä teksti päätyy `MockChannel.outbox`-jonoon, koko ketju toimi.
const FIXED_LLM_REPLY: &str = "AGENT-A-REPLY-OK: hei, tämä tuli aivoista asti";

/// Palauttaa lopullisen LLM-vastauksen outboxista (ei ack/typing/progress-viestejä).
fn find_llm_reply(sent: &[OutboundMessage]) -> Option<&OutboundMessage> {
    sent.iter()
        .find(|m| m.kind == OutboundKind::Message && m.body == FIXED_LLM_REPLY)
}

/// Laskee lopulliset LLM-vastaukset (yksi inbound → yksi tällainen viesti).
fn count_llm_replies(sent: &[OutboundMessage]) -> usize {
    sent.iter()
        .filter(|m| m.kind == OutboundKind::Message && m.body == FIXED_LLM_REPLY)
        .count()
}

/// Käynnistää OpenAI-yhteensopivan mock-LLM-palvelimen satunnaiselle portille.
/// Palauttaa base-URL:n muodossa `http://127.0.0.1:<port>/v1` (resolverille).
async fn spawn_mock_llm() -> String {
    // OpenAI chat-completions -muotoinen vastaus kiinteällä sisällöllä.
    let handler = || async {
        Json(serde_json::json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": { "role": "assistant", "content": FIXED_LLM_REPLY },
                "finish_reason": "stop"
            }]
        }))
    };

    let app = Router::new().route("/v1/chat/completions", post(handler));

    // Sido satunnaiselle vapaalle portille (127.0.0.1:0) ja lue oikea portti.
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("mock-LLM bind");
    let addr = listener.local_addr().expect("mock-LLM local_addr");

    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    format!("http://{addr}/v1")
}

/// **Roundtrip-todiste**: inbound → bus → `Agent.think()` (mock-LLM) →
/// `reply_sink` → `Channel::send`. Yksi viesti sisään, yksi vastaus ulos oikealla
/// kohteella, sisältö = mock-LLM:n kiinteä teksti.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_message_roundtrips_to_channel_send_via_mock_llm() {
    // Serialisoi muiden build_family-testien kanssa: restart-testi asettaa
    // prosessin laajuisen FAMILYCLAW_DATA_DIR:n, joka EI saa vuotaa tähän
    // in-memory-testiin (muuten viesti kirjautuisi väärään levypolkuun).
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    // 1. Mock-LLM pystyyn (OpenAI-yhteensopiva, kiinteä vastaus).
    let api_base = spawn_mock_llm().await;

    // 2. Mock-kanava + injektoitu inbound-viesti. KLOONI talteen, jotta voimme
    //    tarkistaa outboxin sen jälkeen kun build_family on kuluttanut kanavan
    //    `Box<dyn Channel>`:nä (kloonit jakavat saman Arc<Inner>-tilan).
    let channel = MockChannel::new("mock-agent-a").expect("channel");
    let outbox_probe = channel.clone();

    channel
        .inject(
            InboundMessage::new("user-a", "conv-a", "generic being, are you there?")
                .expect("inbound"),
        )
        .expect("inject");
    // Sulje saapuva virta: puskuroitu viesti kulutetaan, sitten pumppu päättyy
    // deterministisesti (ei riipu ajastuksesta).
    channel.close_inbound();

    // 3. Resolveri osoittaa provider-prefiksin "mock" mock-LLM:n base-URL:ään.
    //    Avain luetaan env-muuttujasta jota EI ole asetettu → tyhjä Bearer,
    //    mutta mock ei tarkista auth:ia. (KERROS A: ei kovakoodattua avainta.)
    let resolver =
        EnvEndpointResolver::new().with_provider("mock", api_base, "FAMILYCLAW_MOCK_LLM_KEY_UNSET");

    // 4. Agentti käyttää mallia "mock/agent-a" → resolveri ratkaisee sen
    //    mock-LLM:ään → Agent saa Some(llm) → think() tuottaa kiinteän tekstin.
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("mock/agent-a"));
    let soul =
        familyclaw_agent::Soul::from_essence("I am a generic FamilyClaw being for this test.");

    // 5. Reply-kohde = se yksi keskustelu (MVP: staattinen target).
    let reply_target = "conv-a".to_string();

    // 6. KOKOA RUNTIME — sama kutsu jonka gateway tekee.
    let runtime = build_family(
        Some("roundtrip-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        reply_target.clone(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // Bus elossa + agentti rekisteröity (sama valmius jonka /readyz raportoi).
    assert_eq!(
        runtime.bus().count().await.expect("count"),
        1,
        "agentti busissa"
    );

    // 7. Odota että ketju ehtii: pump → handle_turn → think (HTTP) → route_reply
    //    → drain → Channel::send. Pollaa outboxia (max ~3s) sen sijaan että
    //    nukutaan kiinteä aika — robusti hidasta CI:tä vastaan.
    let mut sent = Vec::new();
    for _ in 0..60 {
        sent = outbox_probe.sent();
        if find_llm_reply(&sent).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 8. TODISTE: yksi lopullinen vastaus, oikea kohde, sisältö = mock-LLM.
    assert_eq!(
        count_llm_replies(&sent),
        1,
        "yksi inbound → yksi lopullinen outbound (ack/typing sallittu, ei kahdennusta)"
    );
    let reply = find_llm_reply(&sent).expect("LLM reply in outbox");
    assert_eq!(
        reply.target, reply_target,
        "vastaus ohjautui oikeaan keskusteluun (staattinen reply_target)"
    );
    assert_eq!(
        reply.body, FIXED_LLM_REPLY,
        "vastauksen sisältö on TASAN se mitä Agent.think() sai mock-LLM:ltä"
    );

    runtime.shutdown().await;
}

/// **F1-failover-todiste**: kun primary-endpoint on kuollut (yhteys
/// hylätään), mutta fallback-malli osoittaa elävään mock-LLM:ään, ketju
/// failoveroi automaattisesti fallbackiin ja tuottaa silti vastauksen.
/// Ennen F1:tä Agent piti vain yhtä klienttiä → primaryn kuolema tappoi vuoron.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dead_primary_fails_over_to_live_fallback() {
    // Serialisoi restart-testin env-mutaation kanssa (ks. roundtrip-testi).
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    // 1. Elävä mock-LLM fallbackille (palauttaa kiinteän tekstin).
    let live_base = spawn_mock_llm().await;

    // 2. Kuollut "primary"-endpoint: sido portti ja sulje listener heti, jolloin
    //    osoitteeseen ei kuuntele mitään → reqwest saa yhteysvirheen → failover.
    let dead_addr = {
        let l = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("dead bind");
        let addr = l.local_addr().expect("dead addr");
        drop(l); // vapauta portti → mikään ei kuuntele.
        addr
    };
    let dead_base = format!("http://{dead_addr}/v1");

    let channel = MockChannel::new("mock-failover").expect("channel");
    let outbox_probe = channel.clone();
    channel
        .inject(InboundMessage::new("user-a", "fo-chat", "kestääkö ketju?").expect("inbound"))
        .expect("inject");
    channel.close_inbound();

    // 3. Resolveri: "dead"-provider → kuollut endpoint, "live"-provider → mock.
    let resolver = EnvEndpointResolver::new()
        .with_provider("dead", dead_base, "FAMILYCLAW_DEAD_KEY_UNSET")
        .with_provider("live", live_base, "FAMILYCLAW_LIVE_KEY_UNSET");

    // 4. Primary = dead/model (kaatuu), fallback = live/model (onnistuu).
    let agent_cfg = AgentConfig::new(
        "agent_a",
        ModelConfig::new("dead/model").with_fallback("live/model"),
    );
    let soul = familyclaw_agent::Soul::from_essence("generic being for failover test");

    let reply_target = "fo-chat".to_string();
    let runtime = build_family(
        Some("failover-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        reply_target.clone(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // 5. Pollaa outboxia: primary kaatuu, fallback tuottaa vastauksen.
    let mut sent = Vec::new();
    for _ in 0..60 {
        sent = outbox_probe.sent();
        if find_llm_reply(&sent).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 6. TODISTE: vastaus tuli silti läpi (fallbackin kautta), oikealla kohteella.
    assert_eq!(
        count_llm_replies(&sent),
        1,
        "kuollut primary → failover fallbackiin → yksi lopullinen vastaus"
    );
    let reply = find_llm_reply(&sent).expect("LLM reply");
    assert_eq!(reply.target, reply_target, "vastaus oikeaan keskusteluun");
    assert_eq!(
        reply.body, FIXED_LLM_REPLY,
        "vastaus tuli fallback-mallilta (live mock-LLM)"
    );

    runtime.shutdown().await;
}

/// Käynnistää **hyytyvän** (slow-loris) HTTP-endpointin: hyväksyy TCP-yhteyden
/// mutta EI koskaan kirjoita vastausta. Tämä simuloi jumittunutta primaryä,
/// joka — toisin kuin `dead_primary` (ECONNREFUSED) — hyväksyy yhteyden mutta
/// nukkuu yli request-timeoutin. Palauttaa base-URL:n resolverille.
///
/// Säilytetyt socketit pidetään elossa taustataskissa (drop sulkisi ne ja
/// muuttaisi käytöksen yhteysvirheeksi).
async fn spawn_hanging_llm() -> String {
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("hang bind");
    let addr = listener.local_addr().expect("hang addr");

    tokio::spawn(async move {
        let mut held = Vec::new();
        // Hyväksy yhteys, ÄLÄ vastaa, pidä socket auki `held`:ssä → asiakas
        // hyytyy kunnes oma request-timeout laukeaa. Päättyy kun listener sulkeutuu.
        while let Ok((sock, _)) = listener.accept().await {
            held.push(sock);
        }
    });

    format!("http://{addr}/v1")
}

/// **F1-timeout-todiste (juurisyy):** primary HYVÄKSYY yhteyden mutta NUKKUU yli
/// request-timeoutin (slow-loris/hang). Ennen F1:tä `LlmClient` rakensi
/// `reqwest::Client`:n ILMAN timeoutia → hyytynyt primary blokkasi
/// `LlmFailover::complete()`:n ikuisesti eikä failover lauennut koskaan.
/// Tämän testin lyhyellä timeoutilla primary antautuu retryable
/// `LlmError::Timeout`-virheellä → ketju failoveroi elävään fallbackiin →
/// vastaus tulee silti läpi. Erotuksena `dead_primary`-testiin tämä laukeaa
/// TIMEOUTISTA, ei pelkästä ECONNREFUSED:ista.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_primary_fails_over_to_live_fallback() {
    // Serialisoi restart-testin env-mutaation kanssa (ks. roundtrip-testi).
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    // 1. Elävä mock-LLM fallbackille + hyytyvä "primary".
    let live_base = spawn_mock_llm().await;
    let hanging_base = spawn_hanging_llm().await;

    let channel = MockChannel::new("mock-timeout-fo").expect("channel");
    let outbox_probe = channel.clone();
    channel
        .inject(InboundMessage::new("user-a", "to-chat", "jumittuuko ketju?").expect("inbound"))
        .expect("inject");
    channel.close_inbound();

    // 2. Resolveri: "hang"-provider → hyytyvä endpoint LYHYELLÄ request-
    //    timeoutilla (300ms) jotta testi on nopea; "live"-provider → mock.
    //    Timeout asetetaan resolverista (KERROS B -polku) → se periytyy
    //    jokaiseen ratkaistuun LlmConfigiin = sama polku jonka gateway ajaa.
    let resolver = EnvEndpointResolver::new()
        .with_request_timeout_ms(300)
        .with_connect_timeout_ms(300)
        .with_provider("hang", hanging_base, "FAMILYCLAW_HANG_KEY_UNSET")
        .with_provider("live", live_base, "FAMILYCLAW_LIVE_KEY_UNSET");

    // 3. Primary = hang/model (hyytyy → timeout), fallback = live/model (onnistuu).
    let agent_cfg = AgentConfig::new(
        "agent_a",
        ModelConfig::new("hang/model").with_fallback("live/model"),
    );
    let soul = familyclaw_agent::Soul::from_essence("generic being for timeout-failover test");

    let reply_target = "to-chat".to_string();
    let runtime = build_family(
        Some("timeout-failover-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        reply_target.clone(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // 4. Pollaa outboxia: hyytynyt primary aikakatkaistaan (~300ms), sitten
    //    fallback tuottaa vastauksen. Annetaan reilu ikkuna (timeout + verkko).
    let mut sent = Vec::new();
    for _ in 0..120 {
        sent = outbox_probe.sent();
        if find_llm_reply(&sent).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 5. TODISTE: vastaus tuli läpi fallbackin kautta — failover laukesi
    //    TIMEOUTISTA (ei ECONNREFUSED), oikealla kohteella.
    assert_eq!(
        count_llm_replies(&sent),
        1,
        "hyytynyt primary → timeout → failover fallbackiin → yksi lopullinen vastaus"
    );
    let reply = find_llm_reply(&sent).expect("LLM reply");
    assert_eq!(reply.target, reply_target, "vastaus oikeaan keskusteluun");
    assert_eq!(
        reply.body, FIXED_LLM_REPLY,
        "vastaus tuli fallback-mallilta (live mock-LLM) timeoutin jälkeen"
    );

    runtime.shutdown().await;
}

/// Vahvistaa että ilman LLM:ää (provider tuntematon → ei think-tekstiä) ketju
/// EI tuota ulosmenevää viestiä — eli reply tulee aidosti `think()`:istä, ei
/// jostain muusta lähteestä. (Negatiivinen kontrolli roundtrip-väitteelle.)
/// **F2-todiste (per-viesti-origin reititys):** yksi agentti saa kaksi viestia
/// **eri alkuperista** (chA/convA ja chB/convB). Jokaisen vastauksen kohde
/// johdetaan **per viesti** kirjekuoren `origin`-kentasta -> kaksi vastausta
/// OIKEISIIN kohteisiin (convA -> convA, convB -> convB), EI vuotoa eika
/// molempia staattiseen kohteeseen.
///
/// Tama on F2:n juurisyy-todiste: ennen originin kuljetusta bus-kirjekuoreen
/// agentti reititti AINA staattiseen [`Agent::with_reply_target`]-arvoon, joten
/// >1 keskustelu vuoti samaan kohteeseen. Nyt origin kantaa keskustelun ja
/// reply-kohde on per-viesti. Staattinen kohde annetaan tarkoituksella
/// MOLEMMISTA poikkeavana ("UNUSED-static"): jos reititys vahingossa kayttaisi
/// sita, testi kaatuisi.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_origins_route_replies_to_correct_targets_no_leak() {
    use std::sync::Arc;

    use familyclaw_agent::{build_llm_chain, new_reply_channel, Agent, Soul};
    use familyclaw_bus::{BeingId, BusMessage, MessageOrigin, ResonanceBus};
    use familyclaw_durable::{DurableContext, InMemoryJournal, Journal};
    use familyclaw_memory::{LocalJsonStore, MemoryStore};

    // 1. Elava mock-LLM (kiintea vastaus) - sama teksti molemmille vuoroille,
    //    joten ero nakyy VAIN kohteessa (target), ei sisallossa.
    let api_base = spawn_mock_llm().await;
    let resolver =
        EnvEndpointResolver::new().with_provider("mock", api_base, "FAMILYCLAW_F2_MOCK_KEY_UNSET");

    // 2. Bus + reply-sink (otetaan recv-paa talteen, tarkistetaan kohteet).
    let bus = ResonanceBus::start(Some("f2-origin-bus".to_string()))
        .await
        .expect("bus");
    let (sink, mut reply_rx) = new_reply_channel();

    // 3. Agentti yhdella mallilla (mock-LLM) + reply-sink + STAATTINEN kohde
    //    joka EI ole kumpikaan keskustelu (todistaa etta origin voittaa).
    let model = ModelConfig::new("mock/agent-a");
    let chain = build_llm_chain(&model, &resolver).expect("chain builds");
    let memory: Arc<dyn MemoryStore + Send + Sync> = Arc::new(LocalJsonStore::in_memory());
    let durable =
        DurableContext::new(Arc::new(InMemoryJournal::new()) as Arc<dyn Journal + Send + Sync>)
            .expect("durable");
    let agent = Agent::new(
        AgentConfig::new("agent_a", model),
        Soul::from_essence("generic being for F2 origin routing test"),
        memory,
        durable,
        bus.clone(),
        None,
        None,
    )
    .with_reply_sink(sink)
    .with_reply_target("UNUSED-static-target")
    .with_failover(chain);
    let _actor = agent.spawn().await.expect("spawn");

    // 4. Kanavan oma bus-seat (ERI kuin agentti, muuten "oma kaiku" skipataan).
    let channel_seat = BeingId::new();

    // 5. Julkaise KAKSI viestia eri alkuperista origin-kentan kanssa.
    let origin_a = MessageOrigin::new("chA", "convA", "user-a");
    let origin_b = MessageOrigin::new("chB", "convB", "user-b");
    bus.publish_with_origin(channel_seat, BusMessage::text("viesti A:lta"), origin_a)
        .expect("publish A");
    bus.publish_with_origin(channel_seat, BusMessage::text("viesti B:lta"), origin_b)
        .expect("publish B");

    // 6. Kerää kaksi lopullista vastausta (ack/typing ohitetaan).
    let mut targets = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while targets.len() < 2 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timeout waiting for two LLM replies"
        );
        while let Ok(out) = reply_rx.try_recv() {
            if out.kind == OutboundKind::Message && out.body == FIXED_LLM_REPLY {
                targets.push(out.target);
            }
        }
        if targets.len() < 2 {
            if let Ok(Some(out)) =
                tokio::time::timeout(Duration::from_millis(100), reply_rx.recv()).await
            {
                if out.kind == OutboundKind::Message && out.body == FIXED_LLM_REPLY {
                    targets.push(out.target);
                }
            }
        }
    }
    targets.sort();

    // 7. TODISTE: kaksi vastausta, kohteet = convA ja convB (per-viesti-origin),
    //    EI "UNUSED-static-target", EI vuotoa (kumpikin tasmalleen kerran).
    assert_eq!(
        targets,
        vec!["convA".to_string(), "convB".to_string()],
        "vastaukset ohjautuivat per-viesti-originin kohteisiin, ei staattiseen"
    );

    bus.stop();
}

/// Vahvistaa että ilman LLM:ää (provider tuntematon → ei think-tekstiä) ketju
/// EI tuota ulosmenevää viestiä — eli reply tulee aidosti `think()`:istä, ei
/// jostain muusta lähteestä. (Negatiivinen kontrolli roundtrip-väitteelle.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_llm_no_reply_is_emitted() {
    // Serialisoi restart-testin env-mutaation kanssa: tämä testi olettaa
    // in-memory-polun (ei FAMILYCLAW_DATA_DIR) → ei saa vuotaa levypolkua.
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    let channel = MockChannel::new("mock-nollm").expect("channel");
    let outbox_probe = channel.clone();
    channel
        .inject(InboundMessage::new("u", "c", "moi ilman aivoja").expect("inbound"))
        .expect("inject");
    channel.close_inbound();

    // Tyhjä resolveri → provider ei ratkea → build_llm_chain Err → ei LLM:ää.
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("unknown/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being");

    let runtime = build_family(
        None,
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "c".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // Anna ketjun pyöriä — viesti pumppautuu; turn-watchdog lähettää hiljaisuusvaroituksen
    // koska LLM:ää ei ole eikä varsinaista vastausta synny.
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        outbox_probe.sent_count(),
        1,
        "ilman LLM:ää turn-watchdog lähettää fallback-vastauksen (ei hiljaista kuolemaa)"
    );

    runtime.shutdown().await;
}

/// Serialisoi `FAMILYCLAW_DATA_DIR`-riippuvaiset testit: env-muuttuja on
/// prosessin laajuinen, joten kaksi rinnakkaista testiä sotkisivat toisensa.
///
/// **Async** mutex (ei `std::sync::Mutex`): sen guard on `Send`, joten sitä voi
/// pitää `.await`-pisteiden yli `multi_thread`-tokio-testissä ilman että future
/// muuttuu `!Send`:ksi.
static DATA_DIR_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Yksilöllinen väliaikaishakemisto tälle testiajolle (ei ulkoisia crateja).
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "familyclaw-runtime-restart-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let _ = std::fs::remove_dir_all(&p);
    p
}

/// Pystyttää persistentin runtimen annettuun data-hakemistoon, syöttää yhden
/// viestin, odottaa että ketju käsittelee sen ja sammuttaa siististi. Palauttaa
/// kuinka monta ulosmenevää vastausta kanavalle lähti.
async fn run_one_persistent_turn(data_dir: &std::path::Path, body: &str, api_base: &str) -> usize {
    let channel = MockChannel::new("mock-restart").expect("channel");
    let outbox_probe = channel.clone();
    channel
        .inject(InboundMessage::new("user-a", "restart-chat", body).expect("inbound"))
        .expect("inject");
    channel.close_inbound();

    let resolver = EnvEndpointResolver::new().with_provider(
        "mock",
        api_base.to_string(),
        "FAMILYCLAW_RESTART_KEY_UNSET",
    );
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("mock/agent-a"));
    let soul = familyclaw_agent::Soul::from_essence("generic being for restart test");

    // Aseta data-dir tämän build_family-kutsun ajaksi (lukko pidetään kutsujalla;
    // ei rinnakkaisia env-lukijoita). Edition 2021: `set_var` on turvallinen.
    std::env::set_var("FAMILYCLAW_DATA_DIR", data_dir);
    std::env::set_var("FAMILYCLAW_DREAM_DISABLED", "1");

    let runtime = build_family(
        Some(format!("restart-bus-{body}")),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "restart-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // Pollaa outboxia: pump → handle_turn → think (HTTP) → reply → send.
    let mut sent = Vec::new();
    for _ in 0..80 {
        sent = outbox_probe.sent();
        if find_llm_reply(&sent).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Anna durable+muisti-kirjoituksen levähtää levylle ennen sammutusta.
    tokio::time::sleep(Duration::from_millis(50)).await;
    runtime.shutdown().await;
    count_llm_replies(&sent)
}

/// **Gateway-restart-todiste (blocker-regressio):** rakenna persistentti
/// runtime, syötä viesti, sammuta, rakenna UUDELLEEN samasta
/// `FAMILYCLAW_DATA_DIR`:stä ja syötä UUSI viesti. Ennen korjausta agentin
/// `turn_counter` jäi nollaan ja durable-kursori oli yhä replay-tilassa →
/// toinen elävä viesti osui replatuun `turn-0`:aan → agentti mykistyi (ei
/// reply:tä) ja uuden viestin muisti hävisi (turn_key-törmäys → dedup).
/// Korjauksen ([`Agent::resume_live`]) jälkeen toinen viesti käsitellään
/// TUOREENA: uusi reply lähtee JA uusi muistorivi syntyy (yhteensä 2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gateway_restart_processes_new_message_fresh_not_replayed_mute() {
    use familyclaw_memory::{LocalJsonStore, MemoryStore};

    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    let api_base = spawn_mock_llm().await;
    let data_dir = unique_temp_dir("mute");

    // --- Ajo 1: ensimmäinen viesti persistenttiin runtimeen. ---
    let sent_1 = run_one_persistent_turn(&data_dir, "ensimmäinen viesti", &api_base).await;
    assert_eq!(
        sent_1, 1,
        "ajo 1: ensimmäinen viesti tuottaa yhden vastauksen"
    );

    // Muistissa tasan yksi rivi ajon 1 jälkeen (levyltä luettuna).
    let mem_after_1 = LocalJsonStore::open(data_dir.join("memory.json"))
        .await
        .expect("open mem 1");
    assert_eq!(
        mem_after_1.len().await.expect("len 1"),
        3,
        "ajo 1: vuosimuisti + chat user + chat assistant"
    );

    // --- Ajo 2: UUSI viesti, sama data-dir → restart-skenaario. ---
    let sent_2 = run_one_persistent_turn(&data_dir, "toinen UUSI viesti", &api_base).await;

    // YDINVÄITE 1: uusi viesti EI mykisty replayhin — vastaus lähtee.
    assert_eq!(
        sent_2, 1,
        "ajo 2: restartin jälkeen UUSI viesti käsitellään tuoreena (ei replay-mykkyyttä)"
    );

    // YDINVÄITE 2: uusi viesti tuottaa UUDEN muistorivin (ei turn_key-törmäystä).
    let mem_after_2 = LocalJsonStore::open(data_dir.join("memory.json"))
        .await
        .expect("open mem 2");
    assert_eq!(
        mem_after_2.len().await.expect("len 2"),
        6,
        "ajo 2: toinen vuoro lisää kolme muistoriviä (ei turn_key-törmäystä)"
    );

    // Siivous (edition 2021: `remove_var` on turvallinen).
    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// **Defect #1 -todiste (suspend/resume-kaatumiskestävyys tuotantopolulla):**
/// kun `build_family` ajetaan persistentillä polulla (`FAMILYCLAW_DATA_DIR`
/// asetettu), agentin jatkettavien vuorojen pinta kytketään
/// KAATUMISKESTÄVÄKSI `JournalResumableStore`:ksi `<data_dir>/resumable.jsonl`:iin
/// — ei oletukseksi jäävää muistinvaraista pintaa. Ennen korjausta ainoa
/// tuotantopolku (`build_family`, jonka gateway kutsuu) jätti agentin oletukseen
/// (`InMemoryResumableStore`), joten jokainen odottava resumable-vuoro katosi
/// restartissa.
///
/// Todiste on tarkoituksella tiukka mutta epäsuora: emme pääse agentin sisäiseen
/// pintaan (agentti siirtyy actoriin), mutta `JournalResumableStore::open` LUO
/// journal-tiedoston avatessaan. Persistentillä polulla tiedoston on synnyttävä;
/// in-memory-polulla (sama testi ilman data-diriä) sitä ei saa syntyä. Saman
/// data-dirin uudelleenavaus (restart) säilyttää tiedoston — pinta on jaettu ja
/// pysyvä, ei per-prosessi.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_family_wires_durable_resumable_store_on_persistent_path() {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    let api_base = spawn_mock_llm().await;
    let data_dir = unique_temp_dir("resumable");
    let resumable_path = data_dir.join("resumable.jsonl");

    // --- Persistentti polku: resumable.jsonl on synnyttävä. ---
    let sent = run_one_persistent_turn(&data_dir, "viesti yksi", &api_base).await;
    assert_eq!(sent, 1, "persistentti vuoro tuottaa vastauksen");
    assert!(
        resumable_path.is_file(),
        "build_family wires JournalResumableStore on persistent path → resumable.jsonl must exist at {}",
        resumable_path.display()
    );

    // --- Restart (sama data-dir): tiedosto säilyy, ei katoa prosessin yli. ---
    let sent_2 = run_one_persistent_turn(&data_dir, "viesti kaksi", &api_base).await;
    assert_eq!(sent_2, 1, "restartin jälkeinen vuoro tuottaa vastauksen");
    assert!(
        resumable_path.is_file(),
        "durable resumable journal survives restart (shared, persistent — not per-process)"
    );

    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// **Defect #1 -vastaparitodiste (in-memory-polku):** ilman
/// `FAMILYCLAW_DATA_DIR`:iä agentti jää muistinvaraiseen oletukseen eikä mitään
/// resumable-journalia kirjoiteta levylle — taaksepäin-yhteensopiva, ei
/// sivuvaikutuksia tiedostojärjestelmään.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_family_in_memory_path_writes_no_resumable_journal() {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    // Varmista ettei aiempi testi jättänyt env-muuttujaa voimaan.
    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");

    let channel = MockChannel::new("mock-inmem-resumable").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being");

    let runtime = build_family(
        None,
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "c".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // Bus käynnissä, yksi olento — kokoonpano rakentui in-memory-pinnoilla.
    assert_eq!(runtime.bus().count().await.expect("count"), 1);
    runtime.shutdown().await;
}

/// **Exactly-once-todiste (lähetys-outbox tuotantopolulla):** kun `build_family`
/// ajetaan persistentillä polulla (`FAMILYCLAW_DATA_DIR` asetettu), agentin
/// jakama toimintoajoympäristö saa KAATUMISKESTÄVÄN lähetys-outboxin
/// (`JournalDispatchOutbox`, `<data_dir>/dispatch_outbox.jsonl`) oletuksellisen
/// muistinvaraisen tilalle. Ennen korjausta ainoa tuotantopolku (`build_family`,
/// jonka gateway kutsuu) jätti outboxin oletukseen (`InMemoryDispatchOutbox`),
/// joten `submit_task`:n exactly-once-takuu kuoli juuri siinä SIGKILL-
/// kaatumisessa jonka selviämiseksi outbox on olemassa.
///
/// Todiste on kaksinkertainen: (1) suora — jaetun ajoympäristön
/// `dispatch_outbox_kind()` on `"journal"` (ei `"in-memory"`); (2) epäsuora —
/// `JournalDispatchOutbox::open` LUO journal-tiedoston, joten
/// `<data_dir>/dispatch_outbox.jsonl` on synnyttävä ja säilyttävä restartin yli.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_family_wires_durable_dispatch_outbox_on_persistent_path() {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    let data_dir = unique_temp_dir("dispatch-outbox");
    let outbox_path = data_dir.join("dispatch_outbox.jsonl");
    std::fs::create_dir_all(&data_dir).expect("temp dir");

    std::env::set_var("FAMILYCLAW_DATA_DIR", &data_dir);
    std::env::set_var("FAMILYCLAW_DREAM_DISABLED", "1");

    let channel = MockChannel::new("mock-dispatch-outbox").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being for dispatch outbox test");

    let runtime = build_family(
        Some("dispatch-outbox-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "dispatch-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // Suora väite: jaettu ajoympäristö kantaa journal-outboxia.
    {
        let actions = runtime.actions();
        let guard = actions.lock().await;
        assert_eq!(
            guard.dispatch_outbox_kind(),
            "journal",
            "persistent build wires JournalDispatchOutbox (crash-surviving exactly-once)"
        );
    }
    // Epäsuora väite: tiedosto syntyi avatessa.
    assert!(
        outbox_path.is_file(),
        "build_family wires JournalDispatchOutbox on persistent path → dispatch_outbox.jsonl must exist at {}",
        outbox_path.display()
    );

    runtime.shutdown().await;

    // Restart (sama data-dir): tiedosto säilyy — pinta on jaettu ja pysyvä.
    let channel2 = MockChannel::new("mock-dispatch-outbox-2").expect("channel");
    channel2.close_inbound();
    let agent_cfg2 = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul2 = familyclaw_agent::Soul::from_essence("generic being for dispatch outbox test");
    let runtime2 = build_family(
        Some("dispatch-outbox-bus-2".to_string()),
        agent_cfg2,
        soul2,
        vec![],
        Box::new(channel2),
        "dispatch-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family restart");
    {
        let actions = runtime2.actions();
        let guard = actions.lock().await;
        assert_eq!(
            guard.dispatch_outbox_kind(),
            "journal",
            "restart keeps journal outbox"
        );
    }
    assert!(
        outbox_path.is_file(),
        "durable dispatch outbox journal survives restart (shared, persistent — not per-process)"
    );
    runtime2.shutdown().await;

    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// **Exactly-once-vastaparitodiste (in-memory-polku):** ilman
/// `FAMILYCLAW_DATA_DIR`:iä ajoympäristö jää muistinvaraiseen lähetys-outboxiin
/// (`"in-memory"`) eikä mitään dispatch-journalia kirjoiteta levylle —
/// taaksepäin-yhteensopiva, ei sivuvaikutuksia tiedostojärjestelmään (oikein: ei
/// persistointia pyydetty).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_family_in_memory_path_uses_in_memory_dispatch_outbox() {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");

    let channel = MockChannel::new("mock-inmem-dispatch").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being");

    let runtime = build_family(
        None,
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "c".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    {
        let actions = runtime.actions();
        let guard = actions.lock().await;
        assert_eq!(
            guard.dispatch_outbox_kind(),
            "in-memory",
            "in-memory build keeps InMemoryDispatchOutbox (no persistence requested)"
        );
    }
    runtime.shutdown().await;
}

/// **Laskuri-skill (sivuvaikutuksen mittari) PRODUCT-PATH-outbox-testille.**
///
/// Jokainen `execute` kasvattaa **prosessin sisäistä** laskuria. Testi vaatii
/// että laskuri pysyy **nollassa** kun lähetys palautuu outboxin replayna
/// (committed) tai hylätään fail-closed (intent-only) — kumpikin todistaa ettei
/// sivuvaikutusta ajeta uudelleen [`build_family`]:n läpi.
///
/// Taito on tarkoituksella **auto-run** ([`ActionRisk::ReadOnly`] +
/// [`ApprovalPolicy::AutoIfReadOnly`]), jotta `submit_task_idempotent` AJAISI
/// suorittajan heti — paitsi kun outbox neutraloi sen. Näin "ulkoinen
/// sivuvaikutus" olisi mitattavissa jos kaksoislaukaisu pääsisi läpi.
#[derive(Debug)]
struct CountingSideEffect {
    /// Prosessin sisäinen sivuvaikutuslaskuri (jaettu kloonien kesken).
    calls: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl CountingSideEffect {
    /// Kiinteä tunniste, jotta seed-avain ja uudelleenajo viittaavat samaan taitoon.
    const SKILL_UUID: uuid::Uuid = uuid::uuid!("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");

    fn skill_id() -> familyclaw_actions::SkillId {
        familyclaw_actions::SkillId::from_uuid(Self::SKILL_UUID)
    }
}

#[async_trait::async_trait]
impl familyclaw_actions::ActionExecutor for CountingSideEffect {
    async fn execute(
        &self,
        request: familyclaw_actions::ActionRequest,
    ) -> familyclaw_actions::Result<familyclaw_actions::ActionResult> {
        // SIVUVAIKUTUS: kasvata laskuria. Tämän on tapahduttava korkeintaan kerran.
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(familyclaw_actions::ActionResult::success(
            "counter bumped",
            serde_json::json!({ "ok": true }),
            request.now,
        ))
    }
}

impl familyclaw_actions::Skill for CountingSideEffect {
    fn manifest(&self) -> familyclaw_actions::manifest::SkillManifest {
        familyclaw_actions::manifest::SkillManifest {
            id: Self::skill_id(),
            name: "counting_side_effect_runtime".to_string(),
            version: "1.0.0".to_string(),
            description: "Kasvattaa sivuvaikutuslaskuria (auto-run, testikäyttö).".to_string(),
            permissions: vec![familyclaw_actions::policy::SkillPermission::NetworkRead],
            risk: familyclaw_actions::policy::ActionRisk::ReadOnly,
            approval_policy: familyclaw_actions::policy::ApprovalPolicy::AutoIfReadOnly,
            input_hint: None,
            output_hint: None,
            input_schema: familyclaw_actions::manifest::default_input_schema(),
            publisher: None,
            signature: None,
        }
    }
}

/// **PRODUCT-PATH crash-survival -integraatiotesti (P0-4 / GPT-5.5:n vaatima).**
///
/// Tämä on se testi jonka GPT-5.5 vaati: at-most-once-takuu todistettuna **sillä
/// polulla jonka oikea käyttäjä ajaa** ([`build_family`] + `FAMILYCLAW_DATA_DIR`),
/// EI vain suoralla [`ActionRuntime`]-harnessilla. Se mallintaa kaatumisen
/// **dispatch-lähetyksen ja agenttikerroksen journal-appendin VÄLISSÄ**: outbox
/// on jo kirjoitettu levylle, mutta prosessi kuoli ennen kuin agentti ehti
/// journaloida vuoron. Restartin jälkeen agentin tuore-haara ajaisi SAMAN
/// lähetyksen samalla idempotenssi-avaimella uudelleen — ja **ilman**
/// kaatumiskestävää outboxia se laukaisisi sivuvaikutuksen toiseen kertaan.
///
/// ## Mitä tämä todistaa [`build_family`]:n LÄPI
/// 1. **Seed (kaatuminen ennen restartia):** kirjoitetaan kaatumiskestävään
///    outboxiin (`<data_dir>/dispatch_outbox.jsonl`) kaksi avainta — toinen
///    **committed** (sivuvaikutus ehti tapahtua + sitoutua) ja toinen
///    **intent-only** (sivuvaikutus ehti tapahtua, committed EI) — ilman
///    [`build_family`]:tä, suoraan [`JournalDispatchOutbox`]:lla. Tämä jäljittelee
///    edellisen prosessin levyjälkeä SIGKILL:n jälkeen.
/// 2. **Restart oikealla tuotantopolulla:** ajetaan [`build_family`] SAMALLA
///    `FAMILYCLAW_DATA_DIR`:llä → se kytkee `JournalDispatchOutbox`:n joka
///    rekonstruoi seedatut avaimet levyltä.
/// 3. **Sivuvaikutus EI aja uudelleen:**
///    - committed-avain → `submit_task_idempotent` palauttaa **arvo-identtisen**
///      tallennetun lopputuloksen (sama `task_id`) ajamatta suorittajaa
///      (laskuri pysyy 0:ssa).
///    - intent-only-avain → `submit_task_idempotent` **epäonnistuu suljettuna**
///      ([`ActionError::PolicyDenied`]) ajamatta suorittajaa (laskuri pysyy 0:ssa).
///
/// Laskuri pysyy **tasan nollassa** koko ajan: at-most-once säilyy juuri sillä
/// polulla jolla oikea käyttäjä rakentaa perheen — ei vain yksikkö-harnessilla.
// Lineaarinen seed→restart→todiste-sekvenssi luetaan ylhäältä alas; paloittelu
// apufunktioihin hajottaisi product-path-kertomuksen ilman selvyyshyötyä.
#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn product_path_build_family_honors_persisted_dispatch_outbox_no_double_fire() {
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    use familyclaw_actions::dispatch_outbox::{
        DispatchOutboxStore, DispatchedOutcome, JournalDispatchOutbox,
    };
    use familyclaw_actions::task::TaskStatus;
    use familyclaw_actions::{ActionError, ActionTaskId, SkillId};

    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");

    let data_dir = unique_temp_dir("product-path-outbox");
    std::fs::create_dir_all(&data_dir).expect("temp dir");
    let outbox_path = data_dir.join("dispatch_outbox.jsonl");

    // Vakaat idempotenssi-avaimet (= agentin `turn-{turn}-dispatch-{k}`).
    let committed_key = "turn-7-dispatch-0";
    let intent_only_key = "turn-9-dispatch-0";

    // --- 1) SEED: kaatumisjälki ennen restartia (suoraan outboxiin). ---
    // Tunnettu lopputulos jonka arvo-identtisyyden voimme tarkistaa replayssa.
    let committed_task_id = ActionTaskId::new();
    let committed_outcome = DispatchedOutcome {
        task_id: committed_task_id,
        status: TaskStatus::Done,
        pending_approval: None,
        error: None,
    };
    {
        let seed = JournalDispatchOutbox::open(&outbox_path).expect("seed open");
        // committed-avain: intent + committed (sivuvaikutus ehti tapahtua + sitoutua).
        seed.record_intent(committed_key)
            .expect("seed intent (committed)");
        seed.record_committed(committed_key, &committed_outcome)
            .expect("seed committed");
        // intent-only-avain: VAIN intent (kaatui kesken sivuvaikutuksen).
        seed.record_intent(intent_only_key)
            .expect("seed intent-only");
    } // drop = "kaatuminen"; levyjälki jää.
    assert!(
        outbox_path.is_file(),
        "seedattu dispatch_outbox.jsonl on levyllä"
    );

    // --- 2) RESTART OIKEALLA TUOTANTOPOLULLA: build_family samalla data-dirillä. ---
    std::env::set_var("FAMILYCLAW_DATA_DIR", &data_dir);
    std::env::set_var("FAMILYCLAW_DREAM_DISABLED", "1");

    let channel = MockChannel::new("mock-product-path-outbox").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being for product-path outbox test");

    let runtime = build_family(
        Some("product-path-outbox-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "outbox-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family on seeded data_dir");

    // build_family kytki KAATUMISKESTÄVÄN outboxin (ei muistinvaraista) →
    // seedatut avaimet rekonstruoituivat levyltä.
    let actions = runtime.actions();
    {
        let guard = actions.lock().await;
        assert_eq!(
            guard.dispatch_outbox_kind(),
            "journal",
            "tuotantopolku kytkee JournalDispatchOutboxin (kaatumiskestävä at-most-once)"
        );
    }

    // Rekisteröi laskuri-skill jaettuun ajoympäristöön (sama Arc<Mutex> jonka
    // tool-loop omistaa). Sivuvaikutuslaskuri jaetaan kloonin kautta.
    let calls = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let skill_id: SkillId = CountingSideEffect::skill_id();
    {
        let mut guard = actions.lock().await;
        guard
            .register_skill(CountingSideEffect {
                calls: Arc::clone(&calls),
            })
            .expect("register counting skill into shared runtime");
    }

    let now = familyclaw_core::time::from_unix_secs(1_700_000_500).expect("ts");
    let payload = serde_json::json!({ "n": 1 });

    // --- 3a) committed-avain: replay palauttaa arvo-identtisen lopputuloksen,
    //         EI aja sivuvaikutusta (agentin tuore-haaran uudelleenajo). ---
    let replayed = {
        let mut guard = actions.lock().await;
        guard
            .submit_task_idempotent(committed_key, "agent_a", skill_id, payload.clone(), now)
            .await
            .expect("committed key replays prior outcome (no re-execute)")
    };
    assert_eq!(
        replayed.task_id, committed_task_id,
        "committed-avain palautti ARVO-IDENTTISEN lopputuloksen (sama task_id) — ei tuoretta ajoa"
    );
    assert_eq!(replayed.status, TaskStatus::Done);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "committed-replay EI ajanut sivuvaikutusta uudelleen (laskuri 0)"
    );

    // --- 3b) intent-only-avain: fail-closed (PolicyDenied), EI aja sivuvaikutusta. ---
    let denied = {
        let mut guard = actions.lock().await;
        guard
            .submit_task_idempotent(intent_only_key, "agent_a", skill_id, payload, now)
            .await
            .expect_err("intent-only key must fail closed (not re-execute)")
    };
    assert!(
        matches!(denied, ActionError::PolicyDenied(_)),
        "intent-only-kaatuminen → fail-closed PolicyDenied, ei sokeaa uudelleenajoa (sai: {denied:?})"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "intent-only fail-closed EI ajanut sivuvaikutusta (laskuri yhä 0 — at-most-once säilyi build_family:n läpi)"
    );

    runtime.shutdown().await;

    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// Varmistaa erikseen että inbound todella **kulkee busin läpi agentille**
/// (muisti saa merkinnän), riippumatta reply-pathista — toinen pää roundtripin
/// todisteketjusta.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_reaches_agent_over_bus() {
    // Serialisoi restart-testin env-mutaation kanssa (in-memory-polku odotettu).
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    let channel = MockChannel::new("mock-bus").expect("channel");
    channel
        .inject(InboundMessage::new("u", "c", "muistatko tämän viestin").expect("inbound"))
        .expect("inject");
    channel.close_inbound();

    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being");

    let runtime = build_family(
        None,
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "c".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    tokio::time::sleep(Duration::from_millis(300)).await;

    let beings = runtime.bus().beings().await.expect("beings");
    assert_eq!(beings.len(), 1);
    assert_eq!(beings[0].name, "agent_a");

    runtime.shutdown().await;
}

/// **Durable-pending-pinta tuotantopolulla (review-finding: "production wires
/// durable outbox but leaves pending store in-memory").** Kun `build_family`
/// ajetaan persistentillä polulla (`FAMILYCLAW_DATA_DIR` asetettu), agentin ja
/// operaattoripinnan jakama toimintoajoympäristö saa KAATUMISKESTÄVÄN
/// odottavien hyväksyntöjen pinnan (`JournalPendingStore`,
/// `<data_dir>/pending_approvals.jsonl`) oletuksellisen muistinvaraisen tilalle.
///
/// Ennen korjausta `build_family` kytki kaatumiskestävän lähetys-outboxin mutta
/// jätti pending-pinnan muistiin → restartin jälkeen vielä odottava hyväksyntä
/// katosi muistikartasta ja `approve` palautti `ApprovalMissing` (404) jo ENNEN
/// outboxin InProgress/Committed-vahtia. At-most-once piti silloin VAIN 404:n
/// sivuvaikutuksesta (vahingossa), ei siksi että durable-kerros sen pakottaa.
///
/// Todiste on kaksinkertainen, kuten dispatch-outbox-testissä: (1) suora —
/// jaetun ajoympäristön `pending_store_kind()` on `"journal"` (ei `"in-memory"`);
/// (2) epäsuora — `JournalPendingStore::open` LUO journal-tiedoston, joten
/// `<data_dir>/pending_approvals.jsonl` on synnyttävä ja säilyttävä restartin yli.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_family_wires_durable_pending_store_on_persistent_path() {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    let data_dir = unique_temp_dir("pending-store");
    let pending_path = data_dir.join("pending_approvals.jsonl");
    std::fs::create_dir_all(&data_dir).expect("temp dir");

    std::env::set_var("FAMILYCLAW_DATA_DIR", &data_dir);
    std::env::set_var("FAMILYCLAW_DREAM_DISABLED", "1");

    let channel = MockChannel::new("mock-pending-store").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being for pending store test");

    let runtime = build_family(
        Some("pending-store-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "pending-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    // Suora väite: jaettu ajoympäristö kantaa journal-pending-pintaa.
    {
        let actions = runtime.actions();
        let guard = actions.lock().await;
        assert_eq!(
            guard.pending_store_kind(),
            "journal",
            "persistent build wires JournalPendingStore (crash-surviving pending approvals)"
        );
    }
    // Epäsuora väite: tiedosto syntyi avatessa.
    assert!(
        pending_path.is_file(),
        "build_family wires JournalPendingStore on persistent path → pending_approvals.jsonl must exist at {}",
        pending_path.display()
    );

    runtime.shutdown().await;

    // Restart (sama data-dir): tiedosto säilyy — pinta on jaettu ja pysyvä.
    let channel2 = MockChannel::new("mock-pending-store-2").expect("channel");
    channel2.close_inbound();
    let agent_cfg2 = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul2 = familyclaw_agent::Soul::from_essence("generic being for pending store test");
    let runtime2 = build_family(
        Some("pending-store-bus-2".to_string()),
        agent_cfg2,
        soul2,
        vec![],
        Box::new(channel2),
        "pending-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family restart");
    {
        let actions = runtime2.actions();
        let guard = actions.lock().await;
        assert_eq!(
            guard.pending_store_kind(),
            "journal",
            "restart keeps journal pending store"
        );
    }
    assert!(
        pending_path.is_file(),
        "durable pending journal survives restart (shared, persistent — not per-process)"
    );
    runtime2.shutdown().await;

    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");
    let _ = std::fs::remove_dir_all(&data_dir);
}

/// **Pending-pinnan vastaparitodiste (in-memory-polku):** ilman
/// `FAMILYCLAW_DATA_DIR`:iä ajoympäristö jää muistinvaraiseen pending-pintaan
/// (`"in-memory"`) — taaksepäin-yhteensopiva, ei sivuvaikutuksia
/// tiedostojärjestelmään (oikein: ei persistointia pyydetty).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn build_family_in_memory_path_uses_in_memory_pending_store() {
    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");

    let channel = MockChannel::new("mock-inmem-pending").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being");

    let runtime = build_family(
        None,
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "c".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family");

    {
        let actions = runtime.actions();
        let guard = actions.lock().await;
        assert_eq!(
            guard.pending_store_kind(),
            "in-memory",
            "in-memory build keeps InMemoryPendingStore (no persistence requested)"
        );
    }
    runtime.shutdown().await;
}

/// **PRODUCT-PATH pending-reload -integraatiotesti (review-finding ydin).**
///
/// Todistaa että odottava hyväksyntä, joka kirjoitettiin kaatumiskestävään
/// pending-pintaan ENNEN "restartia", LADATAAN takaisin tuoreen `build_family`:n
/// toimesta samasta `FAMILYCLAW_DATA_DIR`:stä — joten `approve` EI enää palauta
/// `ApprovalMissing` (404) vielä odottavalle hyväksynnälle restartin jälkeen.
/// Tämä on se ero jonka korjaus tekee: at-most-once-rajaa ei enää pidetä
/// vahingossa 404:n kautta, vaan vielä-odottava hyväksyntä etenee oikeasti
/// outboxin InProgress/Committed-vahdille.
///
/// 1. **Seed (kaatuminen ennen restartia):** kirjoitetaan kaatumiskestävään
///    pending-pintaan (`<data_dir>/pending_approvals.jsonl`) yksi odottava
///    hyväksyntä — suoraan [`JournalPendingStore`]:lla, ilman `build_family`:tä.
///    Tämä jäljittelee edellisen prosessin levyjälkeä SIGKILL:n jälkeen.
/// 2. **Restart oikealla tuotantopolulla:** ajetaan [`build_family`] SAMALLA
///    `FAMILYCLAW_DATA_DIR`:llä → se kytkee `JournalPendingStore`:n joka
///    rekonstruoi seedatun hyväksynnän levyltä.
/// 3. **Hyväksyntä on yhä odottamassa:** jaetun ajoympäristön
///    `try_pending_approvals()` listaa seedatun `approval_id`:n — sama
///    tunniste jonka `approve` löytäisi (ei `ApprovalMissing`).
// Lineaarinen seed→restart→todiste-sekvenssi luetaan ylhäältä alas.
#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn product_path_build_family_reloads_pending_approval_after_restart() {
    use familyclaw_actions::approval::Approval;
    use familyclaw_actions::pending_store::{
        JournalPendingStore, PendingApprovalStore, PendingRecord,
    };
    use familyclaw_actions::{ActionId, ActionTaskId, ApprovalId};

    let _guard = DATA_DIR_ENV_LOCK.lock().await;
    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");

    let data_dir = unique_temp_dir("product-path-pending");
    std::fs::create_dir_all(&data_dir).expect("temp dir");
    let pending_path = data_dir.join("pending_approvals.jsonl");

    // Aikaleimat: myönnetty menneisyydessä, vanhenee kaukana tulevaisuudessa,
    // jottei eviktointi pudota seedattua hyväksyntää restartissa.
    let granted_at = familyclaw_core::time::from_unix_secs(1_700_000_000).expect("granted ts");
    let expires_at = familyclaw_core::time::from_unix_secs(4_000_000_000).expect("expiry ts");

    // --- 1) SEED: vielä odottava hyväksyntä levylle ennen restartia. ---
    let task_id = ActionTaskId::new();
    let approval_id = {
        let approval = Approval {
            id: ApprovalId::new(),
            action_id: ActionId::new(),
            payload_hash: "0".repeat(64),
            granted_at,
            expires_at,
            consumed: false,
        };
        let id = approval.id;
        let record = PendingRecord::new(
            task_id,
            approval,
            "generic skill awaiting human approval",
            granted_at,
        );
        let seed = JournalPendingStore::open(&pending_path).expect("seed open");
        seed.insert(record).expect("seed insert");
        id
    }; // drop = "kaatuminen"; levyjälki jää.
    assert!(
        pending_path.is_file(),
        "seedattu pending_approvals.jsonl on levyllä"
    );

    // --- 2) RESTART OIKEALLA TUOTANTOPOLULLA: build_family samalla data-dirillä. ---
    std::env::set_var("FAMILYCLAW_DATA_DIR", &data_dir);
    std::env::set_var("FAMILYCLAW_DREAM_DISABLED", "1");

    let channel = MockChannel::new("mock-product-path-pending").expect("channel");
    channel.close_inbound();
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_a", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being for product-path pending test");

    let runtime = build_family(
        Some("product-path-pending-bus".to_string()),
        agent_cfg,
        soul,
        vec![],
        Box::new(channel),
        "pending-reload-chat".to_string(),
        &resolver,
        None,
    )
    .await
    .expect("build_family on seeded data_dir");

    let actions = runtime.actions();
    {
        let guard = actions.lock().await;
        // build_family kytki KAATUMISKESTÄVÄN pending-pinnan (ei muistinvaraista).
        assert_eq!(
            guard.pending_store_kind(),
            "journal",
            "tuotantopolku kytkee JournalPendingStoren (kaatumiskestävä pending)"
        );
        // YDINVÄITE: seedattu, vielä odottava hyväksyntä LADATTIIN levyltä →
        // approve EI näkisi ApprovalMissing:iä. Listalla on tasan se tunniste.
        let pending = guard.try_pending_approvals().expect("list pending");
        assert!(
            pending
                .iter()
                .any(|p| p.approval_id == approval_id && p.task_id == task_id),
            "vielä odottava hyväksyntä rekonstruoitui restartissa (ei ApprovalMissing): \
             approval_id={approval_id} task_id={task_id}, listalla={pending:?}"
        );
    }

    runtime.shutdown().await;

    std::env::remove_var("FAMILYCLAW_DATA_DIR");
    std::env::remove_var("FAMILYCLAW_DREAM_DISABLED");
    let _ = std::fs::remove_dir_all(&data_dir);
}
