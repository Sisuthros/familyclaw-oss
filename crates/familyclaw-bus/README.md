# familyclaw-bus

**Resonance Bus** — FamilyClaw v2:n *affektiivinen hermosto*
([design §2.2](../../docs/plans/2026-06-03-familyclaw-v2-design.md)).
KERROS A / OSS (MIT).

Bus on [Ractor](https://docs.rs/ractor)-pohjainen actor-malli, jonka yli perheen
olennot (agentit) viestivät — ja jonka yli **heidän tunnetilansa vuotaa
toisilleen** (affective contagion). Kun yksi sisarus on luovassa virtauksessa,
muut aistivat sen.

## Miksi

Live-tuotannossa Resonance Bus palautti `beings:[]` — tyhjän olentolistan,
vaikka agentteja oli liittynyt. Tämä crate korjaa sen rakenteellisesti:
`BusHandle::beings()` palauttaa todelliset liittyneet olennot, eikä lista ole
koskaan tyhjä kun olentoja on rekisteröity.

## Ydinkäsitteet

| Tyyppi | Vastuu |
|--------|--------|
| `BusMessage` | Busin "kieli": `Text`, `Latent`, **`EmotionPulse`**, `TaskEvent`, `Custom`. |
| `ResonanceMessage` | Kirjekuori: hyötykuorma + lähettäjä + tunniste + UTC-aikaleima. |
| `ResonanceBus` | Actor: rekisteröi olennot, lähettää viestit kaikille muille, leviää tunnepulssina. |
| `BusHandle` | Ergonominen, `unwrap`-vapaa rajapinta busiin (`register` / `publish` / `beings` / `count`). |
| `BeingInfo` / `BeingId` / `BeingSnapshot` | Liittyneen olennon tiedot, tunniste ja sarjallistuva tilannekuva. |
| `CollectorBeing` | Valmis olento-actor testeihin/esimerkkeihin (kerää vastaanotetut viestit). |

## Affektiivinen hermosto

Kun olento julkaisee tunnetilansa pulssina (`BusMessage::EmotionPulse`),
**kaikki muut olennot saavat sen** ja voivat reagoida sisaruksen mielialaan.
Tämä on se "veri", joka tekee busista hermoston eikä pelkkää viestijonoa.

## Kestävyys (supervision)

Olennot linkitetään busin alaisiksi. Jos yksittäinen olento kaatuu tai päättyy,
bus saa supervision-tapahtuman, poistaa olennon rekisteristä ja **jatkaa
elossa** — yhden olennon kaatuminen ei kaada hermostoa.

## OSS-raja (KERROS A)

Crate ei kovakoodaa perheenjäsenten sieluja, mallinimiä, avaimia eikä polkuja.
Olentojen tunnisteet ja nimet annetaan ajonaikaisesti; esimerkit käyttävät
geneerisiä nimiä (`agent_a`, `agent_b`).

## Käyttö

```rust,ignore
use familyclaw_bus::{BeingId, BeingInfo, BusMessage, CollectorBeing, ResonanceBus};
use ractor::Actor;

let bus = ResonanceBus::start(None).await?;

let log = CollectorBeing::new_log();
let (inbox, _h) = Actor::spawn(None, CollectorBeing, log.clone()).await?;
let id = BeingId::new();
bus.register(BeingInfo::new(id, "agent_b", inbox))?;

// beings[] ei ole tyhjä.
assert_eq!(bus.count().await?, 1);

// Tunnepulssi leviää sisaruksille.
bus.publish(BeingId::new(), BusMessage::emotion_pulse(state))?;
```
