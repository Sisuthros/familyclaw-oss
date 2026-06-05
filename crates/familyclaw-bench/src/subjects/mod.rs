//! Konkreettiset [`Subject`](crate::Subject)-toteutukset.
//!
//! Tällä hetkellä vain [`FamilyClawSubject`] — joka ajaa `continuity_daemon`
//! -binääriä mustana laatikkona lapsiprosessina (design §2.1). Kilpailija-
//! adapterit (Letta, OpenClaw) tulevat saman [`Subject`](crate::Subject)-
//! rajapinnan taakse uusina moduuleina ilman harness-uudelleensuunnittelua.

pub mod familyclaw;

pub use familyclaw::FamilyClawSubject;
