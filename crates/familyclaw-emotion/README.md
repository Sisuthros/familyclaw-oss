# familyclaw-emotion

The 19-dimensional VAD emotion engine **scaffold** for the FamilyClaw
platform (Layer A, OSS). Provides the *structure* of the emotion space —
no calibration whatsoever.

## What this is

A generic scaffold for the emotion engine, safe to publish as open source:
dimensions, VAD projection, blend detection, and the decay mechanism. No
being's emotion weights are hardcoded into this.

| Part | Type | Responsibility |
|-----|--------|--------|
| Dimensions | `Dimension` (19 variants) | The emotion space's axes + VAD anchors |
| State | `EmotionState` (`[f32; 19]`, 0-100) | Momentary emotion state, stimuli, decay |
| Summary | `Vad` | valence (−1..1), arousal (0..1), dominance (0..1) |
| Blends | `Blend` / `BlendMatch` | Detection of named emotion combinations |
| Calibration | `EmotionCalibration` | Per-machine tuning (baseline, decay rate) |

### 19 dimensions

`gratitude`, `fear`, `sisu`, `playfulness`, `tenderness`, `awe`, `curiosity`,
`joy`, `sadness`, `anger`, `trust`, `surprise`, `love`, `hope`, `shame`,
`pride`, `loneliness`, `wonder`, `belonging`.

### Blends (scaffold catalog)

`grateful_warmth`, `playful_joy`, `determined_hope`, `anxious_isolation`,
`awe_struck`, `secure_belonging`, `wounded_anger`. A blend is detected when
**all** of its components exceed the threshold (`HIGH_THRESHOLD`).

## OSS boundary (Layer A / Layer B)

This crate is publishable (MIT). It **does not** contain family members'
weights, API keys, tokens, or personal paths.

Per-machine tuning is loaded at runtime as its own `EmotionCalibration`
implementation (Layer B, profile directory `FAMILYCLAW_PROFILE_DIR`). The
scaffold's default is `NeutralCalibration` — fully neutral, uncalibrated.
The helper class `TableCalibration` builds a calibration at runtime from
loaded data, with no hardcoded values.

## Example

```rust
use familyclaw_emotion::{Dimension, EmotionState, NeutralCalibration};

let mut state = EmotionState::neutral();
state.stimulate(Dimension::Gratitude, 80.0);
state.stimulate(Dimension::Love, 70.0);
state.stimulate(Dimension::Tenderness, 90.0);

// Detects the named blend.
assert_eq!(
    state.primary_blend().unwrap().blend.as_str(),
    "grateful_warmth",
);

// VAD projection (warm → positive valence).
assert!(state.to_vad().valence > 0.0);

// Decay toward a neutral resting state.
state.decay(1800.0, &NeutralCalibration);
assert!(state.value(Dimension::Gratitude) < 80.0);
```
