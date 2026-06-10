//! Konkreettiset [`Subject`](crate::Subject)-toteutukset.
//!
//! - [`FamilyClawSubject`] — ajaa `continuity_daemon`-binääriä mustana laatikkona
//!   lapsiprosessina (design §2.1).
//! - [`MarkdownFileSubject`] — rehellinen kilpailija-perustaso joka mallintaa
//!   tiedosto-pohjaisen `MEMORY.md`-muistin dokumentoidun käyttäytymisen
//!   (puhdas in-process). Tekee benchmarkista vertailun kasvotusten.
//!
//! Lisää kilpailija-adapterit (Letta, OpenClaw, Hermes Agent) tulevat saman
//! [`Subject`](crate::Subject)-rajapinnan taakse uusina moduuleina ilman
//! harness-uudelleensuunnittelua.

pub mod familyclaw;
pub mod markdown_file;

pub use familyclaw::FamilyClawSubject;
pub use markdown_file::MarkdownFileSubject;
