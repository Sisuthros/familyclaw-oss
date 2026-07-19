//! # familyclaw-scheduler
//!
//! The `FamilyClaw` platform's **minimal interval-based scheduler** (Layer A,
//! OSS). This crate lets you register recurring tool tasks that fire at a
//! fixed interval ([`chrono::Duration`]) and routes every firing through
//! the existing **idempotent dispatch path**
//! ([`familyclaw_actions::ActionRuntime::submit_task_idempotent`]).
//!
//! ```text
//! register(interval) → tick → is it due? → idempotent dispatch → record
//! ```
//!
//! ## Deliberately minimal (roadmap D5)
//! This crate does **not**:
//! - parse cron expressions ([`croner`]) optionally per task,
//! - make LLM calls,
//! - contain any autonomy, consent logic, or "acts on its own" behavior
//!   (a family's governance switches are a different phase, not in this crate),
//! - execute tools itself — it **only routes** dispatch through the
//!   [`familyclaw_actions`] stack's idempotent dispatch.
//!
//! ## Determinism
//! The decision logic (which tasks are due, and with what key) is
//! **pure**: the current instant is supplied as an injected value
//! ([`familyclaw_core::time::Timestamp`]), the clock is never read inside
//! the logic. Only [`runner`] touches real time ([`tokio::time`]). This
//! makes the entire due/key logic unit-testable without real time.
//!
//! ## Idempotency key stability (crash safety)
//! Every firing gets a **deterministic** key of the form
//! `schedule-{task_id}-{epoch_bucket}`, where `epoch_bucket` is
//! `floor(now_unix / interval_secs)` (see [`decision::firing_key`]). The
//! same logical firing window **always produces the same key**, so if the
//! scheduler crashes and restarts within the same window, the dispatch
//! hits the key already committed in the dispatch outbox and the side
//! effect does not fire twice (at-most-once). The key is independent of
//! process memory — it is derived purely from the `task_id`, the
//! interval, and the current instant.
//!
//! ## OSS boundary (Layer A)
//! This crate is publishable. It contains only **generic types** — no
//! real providers, souls, API keys, tokens, or personal paths. Tasks are
//! identified with generic [`familyclaw_actions::SkillId`] and
//! [`ScheduledTaskId`] identifiers.

pub mod decision;
pub mod dispatch;
pub mod persistence;
pub mod runner;
pub mod task;

pub use decision::{firing_key, DueDecision};
pub use dispatch::{DispatchSummary, DueDispatch, Scheduler};
pub use persistence::{AgencyConfig, AgencyScheduledTask};
pub use runner::{run_until_cancelled, SchedulerHandle, SchedulerRunner};
pub use task::{ScheduledTask, ScheduledTaskId};
