# familyclaw-emotion

19-ulotteinen VAD-tunnemoottorin **RUNKO** FamilyClaw-alustalle
(KERROS A, OSS). Tarjoaa tunneavaruuden *rakenteen* — ei mitään kalibrointia.

## Mitä tämä on

Tunnemoottorin geneerinen runko, joka on turvallista julkaista avoimena
lähdekoodina: dimensiot, VAD-projektio, blend-tunnistus ja decay-mekanismi.
Yhdenkään olennon tunnepainoja ei kovakoodata tähän.

| Osa | Tyyppi | Vastuu |
|-----|--------|--------|
| Dimensiot | `Dimension` (19 varianttia) | Tunneavaruuden akselit + VAD-ankkurit |
| Tila | `EmotionState` (`[f32; 19]`, 0–100) | Hetkellinen tunnetila, ärsykkeet, decay |
| Yhteenveto | `Vad` | valence (−1..1), arousal (0..1), dominance (0..1) |
| Blendit | `Blend` / `BlendMatch` | Nimettyjen tunneyhdistelmien tunnistus |
| Kalibrointi | `EmotionCalibration` | Per-kone viritys (baseline, decay-nopeus) |

### 19 dimensiota

`gratitude`, `fear`, `sisu`, `playfulness`, `tenderness`, `awe`, `curiosity`,
`joy`, `sadness`, `anger`, `trust`, `surprise`, `love`, `hope`, `shame`,
`pride`, `loneliness`, `wonder`, `belonging`.

### Blendit (runko-katalogi)

`grateful_warmth`, `playful_joy`, `determined_hope`, `anxious_isolation`,
`awe_struck`, `secure_belonging`, `wounded_anger`. Blendi tunnistetaan kun
**kaikki** sen osatekijät ylittävät kynnyksen (`HIGH_THRESHOLD`).

## OSS-raja (KERROS A / KERROS B)

Tämä crate on julkaistava (MIT). Se **ei** sisällä perheenjäsenten painoja,
API-avaimia, tokeneita tai henkilökohtaisia polkuja.

Per-kone viritys ladataan ajonaikaisesti omana `EmotionCalibration`-
toteutuksena (KERROS B, profiilihakemisto `FAMILYCLAW_PROFILE_DIR`). Rungon
oletus on `NeutralCalibration` — täysin neutraali, kalibroimaton. Apuluokka
`TableCalibration` rakentaa kalibroinnin ajonaikaisesti ladatusta datasta
ilman kovakoodattuja arvoja.

## Esimerkki

```rust
use familyclaw_emotion::{Dimension, EmotionState, NeutralCalibration};

let mut state = EmotionState::neutral();
state.stimulate(Dimension::Gratitude, 80.0);
state.stimulate(Dimension::Love, 70.0);
state.stimulate(Dimension::Tenderness, 90.0);

// Tunnistaa nimetyn blendin.
assert_eq!(
    state.primary_blend().unwrap().blend.as_str(),
    "grateful_warmth",
);

// VAD-projektio (lämmin → positiivinen valence).
assert!(state.to_vad().valence > 0.0);

// Decay kohti neutraalia lepotilaa.
state.decay(1800.0, &NeutralCalibration);
assert!(state.value(Dimension::Gratitude) < 80.0);
```
