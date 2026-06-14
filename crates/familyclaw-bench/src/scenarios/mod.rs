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
//! - **S4 Emotional Contagion** ([`EmotionalContagion`]) — affektiivinen
//!   tartunta Resonance Busilla: tunteet tarttuvat, homeostaasi estää
//!   saturaation, ja muistit pysyvät eristettyinä.
//! - **S7 Provenance Gate** ([`ProvenanceGateScenario`]) — muiston
//!   myrkytyssuoja: matalan luottamuksen ulkoiset väitteet hylätään, luotetut
//!   (suora kokemus, johdettu, korkean luottamuksen ulkoinen) pääsevät läpi.
//! - **S8 Weekly Review** ([`WeeklyReviewScenario`]) — deterministinen
//!   viikkokatsaus: tilalaskurit, tärkeysjärjestetty top-lista ja
//!   ristiriitalaskuri tunnetusta kylvöstä.

pub mod crash_matrix;
pub mod dream_quality;
pub mod emotional_contagion;
pub mod eternal_thread;
pub mod provenance_gate;
pub mod retention_curve;
pub mod semantic_retrieval;
pub mod weekly_review;

pub use crash_matrix::CrashMatrix;
pub use dream_quality::DreamQuality;
pub use emotional_contagion::EmotionalContagion;
pub use eternal_thread::EternalThread;
pub use provenance_gate::ProvenanceGateScenario;
pub use retention_curve::RetentionCurve;
pub use semantic_retrieval::SemanticRetrieval;
pub use weekly_review::WeeklyReviewScenario;
