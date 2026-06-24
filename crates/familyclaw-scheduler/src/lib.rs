//! # familyclaw-scheduler
//!
//! FamilyClaw-alustan **minimaalinen intervalliperustainen ajastin** (KERROS A,
//! OSS). Crate antaa rekisteröidä toistuvia työkalutehtäviä jotka laukeavat
//! kiinteällä aikavälillä ([`chrono::Duration`]) ja reitittää jokaisen
//! laukaisun olemassa olevan **idempotentin lähetyspolun**
//! ([`familyclaw_actions::ActionRuntime::submit_task_idempotent`]) läpi.
//!
//! ```text
//! rekisteröi(interval) → tick → onko erääntynyt? → idempotentti lähetys → kirjaa
//! ```
//!
//! ## Tarkoituksella minimaalinen (roadmap D5)
//! Tämä crate **ei**:
//! - jäsennä cron-lausekkeita (cron on lykätty — vain kiinteä intervalli),
//! - tee LLM-kutsuja,
//! - sisällä mitään autonomiaa, suostumuslogiikkaa eikä "toimii itse" -käytöstä
//!   (perheen hallintakytkimet ovat eri vaihe, eivät tässä cratessa),
//! - suorita työkaluja itse — se **vain reitittää** lähetyksen
//!   [`familyclaw_actions`]-pinon idempotentin lähetyksen kautta.
//!
//! ## Determinismi
//! Päätöslogiikka (mitkä tehtävät erääntyvät ja millä avaimella) on **puhdas**:
//! nykyhetki annetaan injektoituna ([`familyclaw_core::time::Timestamp`]),
//! kelloa ei lueta logiikan sisällä. Vain [`runner`] koskettaa oikeaa aikaa
//! ([`tokio::time`]). Tämä tekee koko erääntymis- ja avainlogiikan
//! yksikkötestattavaksi ilman oikeaa aikaa.
//!
//! ## Idempotenssiavaimen vakaus (kaatumiskestävyys)
//! Jokainen laukaisu saa **deterministisen** avaimen muotoa
//! `schedule-{task_id}-{epoch_bucket}`, jossa `epoch_bucket` on
//! `floor(now_unix / interval_secs)` (ks. [`decision::firing_key`]). Sama
//! looginen laukaisuikkuna tuottaa **aina saman avaimen**, joten jos ajastin
//! kaatuu ja käynnistyy uudelleen saman ikkunan sisällä, lähetys osuu
//! lähetys-outboxissa jo sitoutuneeseen avaimeen eikä sivuvaikutus laukea
//! kahdesti (at-most-once). Avain on riippumaton prosessin muistista — se
//! johdetaan pelkästä `task_id`:stä, intervallista ja nykyhetkestä.
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate on julkaistava. Se sisältää vain **geneerisiä tyyppejä** — ei
//! oikeita providereita, sieluja, API-avaimia, tokeneita eikä henkilökohtaisia
//! polkuja. Tehtävät tunnistetaan geneerisillä [`familyclaw_actions::SkillId`]-
//! ja [`ScheduledTaskId`]-tunnisteilla.

pub mod decision;
pub mod dispatch;
pub mod runner;
pub mod task;

pub use decision::{firing_key, DueDecision};
pub use dispatch::{DispatchSummary, Scheduler};
pub use runner::{run_until_cancelled, SchedulerRunner};
pub use task::{ScheduledTask, ScheduledTaskId};
