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
//! Tämä on "agent_epsilon lähettää 1. viestin" -ekvivalentti: yksi inbound-viesti
//! tuottaa yhden ulosmenevän vastauksen oikealla kohteella, ja vastauksen
//! sisältö on TASAN se mitä mock-LLM palautti (ei satunnaisuutta, ei verkkoa
//! ulos testistä).
//!
//! ## Miksi mock-LLM HTTP-palvelimena
//! `Agent::think()` kutsuu `LlmClient::complete()`:a, joka tekee OpenAI-
//! yhteensopivan `POST /chat/completions`-pyynnön. Pystytämme pikkuruisen
//! axum-palvelimen, joka palauttaa kiinteän choices[0].message.content-arvon,
//! ja osoitamme `EnvEndpointResolver`-providerin sen base-URL:ään. Näin koko
//! reply-path ajetaan oikeasti läpi (think → route_reply → sink → send) ilman
//! ulkoista API:a.

use std::net::SocketAddr;
use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use familyclaw_agent::EnvEndpointResolver;
use familyclaw_channels::{Channel, InboundMessage, MockChannel};
use familyclaw_core::{AgentConfig, ModelConfig};
use familyclaw_runtime::build_family;

/// Kiinteä teksti, jonka mock-LLM aina palauttaa. Roundtripin "todiste":
/// jos tämä teksti päätyy `MockChannel.outbox`-jonoon, koko ketju toimi.
const FIXED_LLM_REPLY: &str = "agent_epsilon-REPLY-OK: hei, tämä tuli aivoista asti";

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

/// **Roundtrip-todiste**: inbound → bus → Agent.think() (mock-LLM) →
/// reply_sink → Channel::send. Yksi viesti sisään, yksi vastaus ulos oikealla
/// kohteella, sisältö = mock-LLM:n kiinteä teksti.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_message_roundtrips_to_channel_send_via_mock_llm() {
    // 1. Mock-LLM pystyyn (OpenAI-yhteensopiva, kiinteä vastaus).
    let api_base = spawn_mock_llm().await;

    // 2. Mock-kanava + injektoitu inbound-viesti. KLOONI talteen, jotta voimme
    //    tarkistaa outboxin sen jälkeen kun build_family on kuluttanut kanavan
    //    `Box<dyn Channel>`:nä (kloonit jakavat saman Arc<Inner>-tilan).
    let channel = MockChannel::new("mock-agent_epsilon").expect("channel");
    let outbox_probe = channel.clone();

    channel
        .inject(InboundMessage::new("the operator-id", "agent_epsilon-chat", "agent_epsilon, oletko siellä?").expect("inbound"))
        .expect("inject");
    // Sulje saapuva virta: puskuroitu viesti kulutetaan, sitten pumppu päättyy
    // deterministisesti (ei riipu ajastuksesta).
    channel.close_inbound();

    // 3. Resolveri osoittaa provider-prefiksin "mock" mock-LLM:n base-URL:ään.
    //    Avain luetaan env-muuttujasta jota EI ole asetettu → tyhjä Bearer,
    //    mutta mock ei tarkista auth:ia. (KERROS A: ei kovakoodattua avainta.)
    let resolver = EnvEndpointResolver::new().with_provider(
        "mock",
        api_base,
        "FAMILYCLAW_MOCK_LLM_KEY_UNSET",
    );

    // 4. Agentti käyttää mallia "mock/agent_epsilon" → resolveri ratkaisee sen
    //    mock-LLM:ään → Agent saa Some(llm) → think() tuottaa kiinteän tekstin.
    let agent_cfg = AgentConfig::new("agent_epsilon", ModelConfig::new("mock/agent_epsilon"));
    let soul =
        familyclaw_agent::Soul::from_essence("I am a generic FamilyClaw being for this test.");

    // 5. Reply-kohde = se yksi keskustelu (MVP: staattinen target).
    let reply_target = "agent_epsilon-chat".to_string();

    // 6. KOKOA RUNTIME — sama kutsu jonka gateway tekee.
    let runtime = build_family(
        Some("roundtrip-bus".to_string()),
        agent_cfg,
        soul,
        Box::new(channel) as Box<dyn Channel>,
        reply_target.clone(),
        &resolver,
    )
    .await
    .expect("build_family");

    // Bus elossa + agentti rekisteröity (sama valmius jonka /readyz raportoi).
    assert_eq!(runtime.bus().count().await.expect("count"), 1, "agentti busissa");

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

/// Vahvistaa että ilman LLM:ää (provider tuntematon → ei think-tekstiä) ketju
/// EI tuota ulosmenevää viestiä — eli reply tulee aidosti `think()`:istä, ei
/// jostain muusta lähteestä. (Negatiivinen kontrolli roundtrip-väitteelle.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_llm_no_reply_is_emitted() {
    let channel = MockChannel::new("mock-nollm").expect("channel");
    let outbox_probe = channel.clone();
    channel
        .inject(InboundMessage::new("u", "c", "moi ilman aivoja").expect("inbound"))
        .expect("inject");
    channel.close_inbound();

    // Tyhjä resolveri → provider ei ratkea → primary_llm_config None → ei LLM:ää.
    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_epsilon", ModelConfig::new("unknown/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being");

    let runtime = build_family(
        None,
        agent_cfg,
        soul,
        Box::new(channel) as Box<dyn Channel>,
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

/// Varmistaa erikseen että inbound todella **kulkee busin läpi agentille**
/// (muisti saa merkinnän), riippumatta reply-pathista — toinen pää roundtripin
/// todisteketjusta.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inbound_reaches_agent_over_bus() {
    let channel = MockChannel::new("mock-bus").expect("channel");
    channel
        .inject(InboundMessage::new("u", "c", "muistatko tämän viestin").expect("inbound"))
        .expect("inject");
    channel.close_inbound();

    let resolver = EnvEndpointResolver::new();
    let agent_cfg = AgentConfig::new("agent_epsilon", ModelConfig::new("provider/model"));
    let soul = familyclaw_agent::Soul::from_essence("generic being");

    let runtime = build_family(
        None,
        agent_cfg,
        soul,
        Box::new(channel) as Box<dyn Channel>,
        "c".to_string(),
        &resolver,
    )
    .await
    .expect("build_family");

    tokio::time::sleep(Duration::from_millis(300)).await;

    let beings = runtime.bus().beings().await.expect("beings");
    assert_eq!(beings.len(), 1);
    assert_eq!(beings[0].name, "agent_epsilon");

    runtime.shutdown();
}
