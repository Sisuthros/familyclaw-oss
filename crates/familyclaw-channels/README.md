# familyclaw-channels

FamilyClaw-alustan **kanavakerros** (KERROS A / OSS — design §3).
Yhtenäinen rajapinta saapuville ja lähteville viesteille sekä silta
Resonance Busiin.

## Vastuu

- **`Channel`-trait** — yhden kaksisuuntaisen kanavan rajapinta:
  - `send(message) -> SendFuture` — lähetä viesti ulos
  - `receive() -> ChannelResult<MessageStream>` — saapuvien viestien virta
  - `channel_id() -> &str` — kanavainstanssin vakaa tunniste
  - `kind() -> ChannelKind` — kanavateknologia
  - Dyn-yhteensopiva: `Box<dyn Channel>` toimii (ilman `async-trait`-makroa).
- **`ChannelKind`** — `Discord` / `Telegram` / `WhatsApp` / `Signal` / `Mock`.
- **Viestityypit** — `OutboundMessage`, `InboundMessage`, `InboundEnvelope`.
- **`MockChannel`** — in-memory testikanava, ei verkkoa eikä ulkoisia SDK:ita.
- **`pump_to`** — integraatiosauma: kanavan virta → Resonance Bus.

## Saapuva viesti → InboundEnvelope → familyclaw_bus::BusMessage

Kanavakerros on Resonance Busin reuna ulkomaailmaan. Saapuva
`InboundMessage` **kanonisoidaan** `InboundEnvelope`-kirjekuoreksi
(`InboundMessage::into_envelope`), joka sisältää:

- yksikäsitteisen `MessageId`:n,
- alkuperän (`ChannelKind` + `channel_id`) vastauksen reititystä varten,
- lähettäjän, keskustelun ja sisällön,
- UTC-aikaleiman (deterministinen durable-replayta varten).

`InboundEnvelope` on tietoisesti **eri tyyppi** kuin busin sisältö-enum
`familyclaw_bus::BusMessage` (näin nimi ei enää törmää yli crate-rajojen) ja
täysin serde-sarjallistuva. Varsinainen `InboundEnvelope →
familyclaw_bus::BusMessage` -muunnos ja julkaisu busiin tehdään
agent-kerroksessa, joka riippuu molemmista crateista.

## Kanava-adapterit ovat feature-flagien takana

Oikeat adapterit vetävät sisään raskaita kanava-SDK:ita, joten ne ovat
feature-flagien takana eivätkä pakollisia riippuvuuksia:

| Feature | Tarkoitus | Esimerkki-SDK |
|---------|-----------|---------------|
| `discord` | Discord-adapteri | serenity |
| `telegram` | Telegram-adapteri | teloxide |
| `whatsapp` | WhatsApp-adapteri | — |
| `signal` | Signal-adapteri | — |

Oletuskäännös (`default = []`) sisältää **vain** rungon + `MockChannel`,
joten alusta kääntyy ja testautuu ilman verkkoa. Kunkin adapterin
SDK-riippuvuus lisätään featuren mukana vasta kun adapteri toteutetaan.

## Käyttö

```rust
use familyclaw_channels::{Channel, ChannelKind, InboundMessage, MockChannel};

#[tokio::main]
async fn main() -> familyclaw_channels::ChannelResult<()> {
    let channel = MockChannel::new("agent-a-mock")?;
    let mut inbound = channel.receive()?;

    // Ulkomaailma syöttää saapuvan viestin → InboundEnvelope.
    channel.inject(InboundMessage::new("user-1", "general", "moi")?)?;
    let envelope = inbound.recv().await.expect("one message");
    assert_eq!(envelope.kind, ChannelKind::Mock);

    // Vastaa samaan keskusteluun.
    channel.send(envelope.reply("hei takaisin")?).await?;
    Ok(())
}
```

### Bus-integraatio (`pump_to`)

```rust,ignore
// Kuluta kanavan virta ja anna jokainen kirjekuore agent-kerroksen
// adapterille, joka muuntaa sen busin hyötykuormaksi ja julkaisee busiin.
familyclaw_channels::pump_to(stream, |envelope| {
    // adapter::publish_envelope(&bus, agent_id, envelope) ...
    Ok(())
}).await?;
```

## OSS-raja

Ei kovakoodattuja kanavatokeneita, Discord-/Telegram-tunnisteita,
palvelin-IP:itä eikä henkilökohtaisia polkuja. Tunnukset ja kohteet ovat
ajonaikaista konfiguraatiota.

## Lisenssi

MIT.
