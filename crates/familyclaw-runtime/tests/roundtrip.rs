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
use familyclaw_channels::{InboundMessage, MockChannel};
use familyclaw_core::{AgentConfig, ModelConfig};
use familyclaw_runtime::build_family;

/// Kiinteä teksti, jonka mock-LLM aina palauttaa. Roundtripin "todiste":
/// jos tämä teksti päätyy `MockChannel.outbox`-jonoon, koko ketju toimi.
const FIXED_LLM_REPLY: &str = "AGENT-A-REPLY-OK: hei, tämä tuli aivoista asti";

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
        Box::new(channel),
        reply_target.clone(),
        &resolver,
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
        if !sent.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 8. TODISTE: tasan yksi ulosmenevä viesti, oikea kohde, sisältö = mock-LLM.
    assert_eq!(
        sent.len(),
        1,
        "yksi inbound → yksi outbound (ei kahdennusta, ei pudotusta)"
    );
    let reply = &sent[0];
    assert_eq!(
        reply.target, reply_target,
        "vastaus ohjautui oikeaan keskusteluun (staattinen reply_target)"
    );
    assert_eq!(
        reply.body, FIXED_LLM_REPLY,
        "vastauksen sisältö on TASAN se mitä Agent.think() sai mock-LLM:ltä"
    );

    runtime.shutdown();
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
        Box::new(channel),
        reply_target.clone(),
        &resolver,
    )
    .await
    .expect("build_family");

    // 5. Pollaa outboxia: primary kaatuu, fallback tuottaa vastauksen.
    let mut sent = Vec::new();
    for _ in 0..60 {
        sent = outbox_probe.sent();
        if !sent.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 6. TODISTE: vastaus tuli silti läpi (fallbackin kautta), oikealla kohteella.
    assert_eq!(
        sent.len(),
        1,
        "kuollut primary → failover fallbackiin → yksi vastaus"
    );
    assert_eq!(sent[0].target, reply_target, "vastaus oikeaan keskusteluun");
    assert_eq!(
        sent[0].body, FIXED_LLM_REPLY,
        "vastaus tuli fallback-mallilta (live mock-LLM)"
    );

    runtime.shutdown();
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
        Box::new(channel),
        reply_target.clone(),
        &resolver,
    )
    .await
    .expect("build_family");

    // 4. Pollaa outboxia: hyytynyt primary aikakatkaistaan (~300ms), sitten
    //    fallback tuottaa vastauksen. Annetaan reilu ikkuna (timeout + verkko).
    let mut sent = Vec::new();
    for _ in 0..120 {
        sent = outbox_probe.sent();
        if !sent.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 5. TODISTE: vastaus tuli läpi fallbackin kautta — failover laukesi
    //    TIMEOUTISTA (ei ECONNREFUSED), oikealla kohteella.
    assert_eq!(
        sent.len(),
        1,
        "hyytynyt primary → timeout → failover fallbackiin → yksi vastaus"
    );
    assert_eq!(sent[0].target, reply_target, "vastaus oikeaan keskusteluun");
    assert_eq!(
        sent[0].body, FIXED_LLM_REPLY,
        "vastaus tuli fallback-mallilta (live mock-LLM) timeoutin jälkeen"
    );

    runtime.shutdown();
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

    // 6. Keraa kaksi vastausta reply-sinkista (timeoutilla, ei kiinteaa unta).
    let mut targets = Vec::new();
    for _ in 0..2 {
        let out = tokio::time::timeout(Duration::from_secs(5), reply_rx.recv())
            .await
            .expect("reply within timeout")
            .expect("reply present");
        targets.push(out.target);
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
        Box::new(channel),
        "c".to_string(),
        &resolver,
    )
    .await
    .expect("build_family");

    // Anna ketjun pyöriä — viesti pumppautuu ja muistetaan, mutta ei reply:ä.
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert_eq!(
        outbox_probe.sent_count(),
        0,
        "ilman LLM:ää think() palauttaa None → ei ulosmenevää vastausta"
    );

    runtime.shutdown();
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
        Box::new(channel),
        "restart-chat".to_string(),
        &resolver,
    )
    .await
    .expect("build_family");

    // Pollaa outboxia: pump → handle_turn → think (HTTP) → reply → send.
    let mut sent = Vec::new();
    for _ in 0..80 {
        sent = outbox_probe.sent();
        if !sent.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Anna durable+muisti-kirjoituksen levähtää levylle ennen sammutusta.
    tokio::time::sleep(Duration::from_millis(50)).await;
    runtime.shutdown();
    sent.len()
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
    assert_eq!(sent_1, 1, "ajo 1: ensimmäinen viesti tuottaa yhden vastauksen");

    // Muistissa tasan yksi rivi ajon 1 jälkeen (levyltä luettuna).
    let mem_after_1 = LocalJsonStore::open(data_dir.join("memory.json"))
        .await
        .expect("open mem 1");
    assert_eq!(
        mem_after_1.len().await.expect("len 1"),
        1,
        "ajo 1: yksi muistorivi"
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
        2,
        "ajo 2: toinen muistorivi syntyi (turn_key ei törmännyt replayn duplikaattiin)"
    );

    // Siivous (edition 2021: `remove_var` on turvallinen).
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
        Box::new(channel),
        "c".to_string(),
        &resolver,
    )
    .await
    .expect("build_family");

    tokio::time::sleep(Duration::from_millis(300)).await;

    let beings = runtime.bus().beings().await.expect("beings");
    assert_eq!(beings.len(), 1);
    assert_eq!(beings[0].name, "agent_a");

    runtime.shutdown();
}
