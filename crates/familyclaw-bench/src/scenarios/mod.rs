//! Continuity scenarios ([`Scenario`](crate::scenario::Scenario) implementations).
//!
//! Each scenario is a deterministic test suite, run against an injected clock,
//! that the harness executes against a [`Subject`](crate::subject::Subject)
//! (design §3):
//!
//! - **S1 Crash Matrix** ([`CrashMatrix`]) — crashing mid-task.
//! - **S2 Retention Curve** ([`RetentionCurve`]) — memory retention over time:
//!   identity anchors (λ=0) persist, trivia decays, and the FamilyClaw model
//!   beats a naive ring-buffer baseline at retaining the memories that matter.
//! - **S4 Emotional Contagion** ([`EmotionalContagion`]) — affective
//!   contagion via the Resonance Bus: emotions spread, homeostasis prevents
//!   saturation, and memories remain isolated.
//! - **S7 Provenance Gate** ([`ProvenanceGateScenario`]) — memory poisoning
//!   defense: low-trust external claims are rejected, while trusted ones
//!   (direct experience, derived, high-trust external) get through.
//! - **S8 Weekly Review** ([`WeeklyReviewScenario`]) — deterministic weekly
//!   review: state counters, an importance-ordered top list, and a conflict
//!   counter from a known seed.

pub mod crash_matrix;
pub mod dream_quality;
pub mod embedding_recall;
pub mod emotional_contagion;
pub mod eternal_thread;
pub mod provenance_gate;
pub mod retention_curve;
pub mod semantic_retrieval;
pub mod weekly_review;

pub use crash_matrix::CrashMatrix;
pub use dream_quality::DreamQuality;
pub use embedding_recall::EmbeddingRecall;
pub use emotional_contagion::EmotionalContagion;
pub use eternal_thread::EternalThread;
pub use provenance_gate::ProvenanceGateScenario;
pub use retention_curve::RetentionCurve;
pub use semantic_retrieval::SemanticRetrieval;
pub use weekly_review::WeeklyReviewScenario;
