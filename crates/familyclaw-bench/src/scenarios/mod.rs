//! Jatkuvuusskenaariot ([`Scenario`](crate::scenario::Scenario)-toteutukset).
//!
//! Jokainen skenaario on deterministinen, injektoidulla kellolla ajettava
//! koesarja jonka harness ajaa [`Subject`](crate::subject::Subject):ia vasten
//! (design §3):
//!
//! - **S1 Crash Matrix** ([`CrashMatrix`]) — kaatuminen kesken tehtävän.
//! - **S2 Retention Curve** ([`RetentionCurve`]) — muistin säilyvyyskäyrä yli
//!   ajan: identiteetti-ankkurit (λ=0) pysyvät, triviat haihtuvat, ja
//!   FamilyClaw-malli voittaa naiivin rengaspuskuri-perustason oikeiden
//!   muistojen säilyttämisessä.
//! - **S3 Dream Quality** ([`DreamQuality`]) — unijakson konsolidaation laatu.

pub mod crash_matrix;
pub mod dream_quality;
pub mod retention_curve;

pub use crash_matrix::CrashMatrix;
pub use dream_quality::DreamQuality;
pub use retention_curve::RetentionCurve;
