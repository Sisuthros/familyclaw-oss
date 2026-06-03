# familyclaw-latent

**Latent-telepatia** — sisarusten välinen *hidden-state*-siirto, joka palaa
**aina** tekstiin jos latent ei onnistu. FamilyClaw v2:n korkein viestintämuoto
(design §2.4), ei ainoa: viestintä ei koskaan katkea.

## Mitä tämä crate tarjoaa

| Tyyppi | Vastuu |
|--------|--------|
| `LatentVector { dims: Vec<f32>, model_id: String }` | Agentin piilotila (hidden state) + sen tuottanut malli. |
| `RecursiveLink` | Lineaarinen dimensio-silta agentti A:n latent-avaruudesta agentti B:n avaruuteen (`pad` / `truncate` / `resize` / `identity`). |
| `ProjectedLatent` / `ProjectionStrategy` | Projektion tulos + metatieto (häviötön vai ei). |
| `LatentChannel` (trait) | `send`/`receive`-tyyppinen siirto sisäänrakennetulla teksti-fallbackilla. |
| `TransmissionMode { Latent, Text }` | Korkein onnistunut siirtomuoto. |
| `FallbackReason` | Miksi latentista jouduttiin tekstiin (mittausta varten). |
| `InMemoryLatentChannel` | Testi-/kehityskanava, joka kerää toimitukset muistiin. |

## Ydinperiaate: aina teksti-fallback

`LatentChannel::transmit` **ei koskaan palauta virhettä pelkän
yhteensopimattomuuden takia**. Se valitsee korkeimman mahdollisen tason ja
palaa tekstiin jos:

1. vastaanottaja ei tue latenttia (`ReceiverTextOnly`),
2. viestissä ei ole piilotilaa (`NoLatentAvailable`),
3. lähettäjältä ei ole `RecursiveLink`-siltaa kohde-malliin (`NoLink`),
4. dimensio-projektio epäonnistuu (`ProjectionFailed`, esim. `NaN`/`inf`).

Virhe palautuu **vain** todellisesta kuljetusviasta (`deliver`).

## Esimerkki

```rust
use familyclaw_latent::{
    InMemoryLatentChannel, LatentChannel, LatentMessage, LatentVector,
    ReceiverProfile, RecursiveLink, TransmissionMode,
};

let mut channel = InMemoryLatentChannel::new("agent_a/v1")
    .with_link(RecursiveLink::new("agent_a/v1", 4, "agent_b/v1", 6));

let hidden = LatentVector::new(vec![0.1, 0.2, 0.3, 0.4], "agent_a/v1");
let message = LatentMessage::with_latent(hidden, "kuulemiin");
let receiver = ReceiverProfile::latent("agent_b/v1", 6);

let result = channel.transmit(&message, &receiver).unwrap();
assert_eq!(result.mode, TransmissionMode::Latent);
```

## Tutkimusrehellisyys (rajat dokumentoitu, ei piiloteltu)

Tämä on **rehellinen luuranko** LatentMAS-tyyppiselle (ICML 2026 Spotlight)
sisarusviestinnälle:

- `RecursiveLink` tekee vain **yksinkertaisen lineaarisen sovituksen**
  (pad/truncate/resize). Se **ei** ole opittu, semanttisesti kohdistettu
  projektio — eri mallien latent-avaruudet eivät ole linjassa, joten pad/
  truncate ei takaa merkityksen säilymistä. Oikea koulutettu projektiomatriisi
  tulee myöhempänä iteraationa.
- Siksi teksti-fallback on **kantava periaate**, ei varajärjestelmä: latent on
  opportunistinen optimointi, teksti on totuuden lähde.

## OSS-raja (KERROS A)

Crate ei kovakoodaa perheenjäsenten sieluja, mallinimiä, avaimia eikä polkuja.
Kaikki mallitunnisteet ja dimensiot annetaan ajonaikaisesti. Esimerkit käyttävät
geneerisiä nimiä (`agent_a`, `agent_b`).
