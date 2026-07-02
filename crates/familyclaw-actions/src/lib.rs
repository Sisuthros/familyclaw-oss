//! # familyclaw-actions
//!
//! FamilyClaw-alustan **toiminto- ja todistepino**: kerros, joka muuntaa
//! agentin aikeen turvalliseksi, todennettavaksi ja muistettavaksi
//! toiminnoksi. Crate toteuttaa seuraavan putken:
//!
//! ```text
//! observe → plan → request approval (jos tarpeen) → execute action
//!         → verify → persist proof → remember → report
//! ```
//!
//! - **observe** — havaitse tilanne ja kerää konteksti.
//! - **plan** — valitse taito ([`registry`]) ja muodosta toimintotehtävä ([`task`]).
//! - **request approval** — pyydä ihmisen hyväksyntä ([`approval`]) jos
//!   käytäntö ([`policy`]) niin vaatii; hyväksyntä on TTL-rajattu,
//!   kertakäyttöinen ja sidottu payload-tiivisteeseen.
//! - **execute** — aja hyväksytty toiminto ([`executor`]) taidon kautta.
//! - **verify** — tarkista tuloksen kelpoisuus jälkiehtoja vasten.
//! - **persist proof** — koosta redaktoitu todistepaketti ([`proof`]).
//! - **remember** — talleta jälki muistiin (substraattikerros erikseen).
//! - **report** — kirjaa audit-tapahtuma ([`audit`]) ja raportoi tulos.
//!
//! Taidot voi myös julkaista MCP-työkaluina ([`mcp`]), ja koko putken
//! end-to-end-käyttäytyminen katetaan arvioinneilla ([`evals`]).
//!
//! ## OSS-raja (KERROS A)
//! Tämä crate on julkaistava. Se sisältää vain **geneerisiä tyyppejä** — ei
//! oikeita providereita, sieluja, API-avaimia, tokeneita, IP-osoitteita eikä
//! henkilökohtaisia polkuja. Mukana on **kaksi aitoa referenssitaitoa**
//! ([`FsReadAllowlisted`] paikallinen tiedostonluku,
//! [`WebFetchSkill`](skills::WebFetchSkill) read-only
//! HTTP-GET SSRF-vartioinnilla) jotka tekevät oikeaa työtä ilman avaimia; loput
//! taidot ovat **esimerkkimalleja** jotka näyttävät taidon sopimuksen ja joihin
//! kytket oman tarjoajasi (ei oikeita Gmail-/GitHub-verkkokutsuja valmiina).
//! Todistepaketit **redaktoivat** salaisuudelta näyttävät arvot ennen
//! tallennusta.
//!
//! ## Suunnitteluperiaatteet
//! - **Ei `unwrap()`/`expect()`/`panic!()` tuotantopolulla.** Kaikki virheet
//!   kulkevat [`ActionError`]- ja [`Result`]-tyyppien kautta.
//! - **Determinismi:** puhdas logiikka ottaa aikaleiman injektoituna
//!   ([`familyclaw_core::time::Timestamp`]) — kelloa ei lueta logiikan sisällä.
//! - **Tyypitetyt tunnisteet** ([`SkillId`], [`ActionTaskId`], [`ApprovalId`],
//!   [`ProofBundleId`], [`ActionId`], [`AuditEventId`]) estävät sekoittamisen
//!   käännösaikana.
//!
//! ## Moduulit
//! - [`manifest`] — skill-manifestit (kuvaus, skeema, capabilityt).
//! - [`registry`] — skill-rekisteri (mock-taidot).
//! - [`policy`] — käytäntö: sallinta + hyväksyntävaatimus.
//! - [`approval`] — ihmisen hyväksyntä (TTL, nonce, payload-sidonta).
//! - [`audit`] — tamper-evident audit-loki.
//! - [`task`] — toimintotehtävän tila ja elinkaari.
//! - [`executor`] — hyväksytyn toiminnon suoritus.
//! - [`proof`] — redaktoitu todistepaketti.
//! - [`mcp`] — taitojen julkaisu MCP-työkaluina.
//! - [`pending_store`] — odottavien hyväksyntöjen kaatumiskestävä tallennuspinta.
//! - [`skills`] — realistiset mock-taidot + koko putken ([`skills::Pipeline`]).
//! - [`facade`] — operaattoripinta ([`ActionRuntime`]) koko putken päälle.
//! - [`evals`] — end-to-end-arvioinnit.
//! - [`ids`] — tyypitetyt tunnisteet.
//! - [`error`] — [`ActionError`], [`Result`].
//!
//! ## Operaattorin komentorivi
//! Crate sisältää myös ohuen komentorivibinäärin
//! (`src/bin/familyclaw-actions-cli.rs`), joka käyttää [`ActionRuntime`]-
//! julkisivua: `list-skills`, `submit-task`, `approve`, `status`, `proof`.

pub mod approval;
pub mod audit;
pub mod dispatch_outbox;
pub mod error;
pub mod evals;
pub mod executor;
pub mod facade;
pub mod ids;
pub mod manifest;
pub mod mcp;
pub mod pending_store;
pub mod policy;
pub mod proof;
pub mod registry;
pub mod resource_budget;
pub mod skills;
pub mod task;

pub use audit::{
    ActionAuditEvent, AuditAction, AuditCollector, AuditKind, AuditLog, ExecAuditEvent,
};
pub use dispatch_outbox::{
    DispatchLookup, DispatchOutboxStore, DispatchedOutcome, InMemoryDispatchOutbox,
    JournalDispatchOutbox,
};
pub use error::{ActionError, Result};
pub use executor::{ActionExecutor, ActionRequest, ActionResult, ActionStatus, MockActionExecutor};
pub use facade::{ActionRuntime, PendingApproval, SkillSummary, SubmitOutcome};
pub use ids::{ActionId, ActionTaskId, ApprovalId, AuditEventId, ProofBundleId, SkillId};
pub use mcp::{
    call_with_policy, McpToolCall, McpToolDescriptor, McpToolProvider, McpToolResult,
    MockMcpProvider,
};
pub use pending_store::{
    DangerousToolRateLimiter, InMemoryPendingStore, JournalPendingStore, PendingApprovalStore,
    PendingCapacity, PendingRecord,
};
pub use proof::{
    build_proof, redact_free_text, redact_value, redact_value_deep, sha256_hex, ProofBundle,
    RedactionReport, VerificationResult,
};
pub use resource_budget::{AcquireOutcome, BudgetLimits, ResourceBudget, ResourceLease};
#[allow(deprecated)]
pub use skills::MockSkill;
pub use skills::{
    DiscordThreadSummaryMock, EmailTriageMock, FilePatchMock, FsReadAllowlisted, FsReadConfig,
    GithubIssueDraftMock, MemoryRecord, Pipeline, PipelineOutcome, Skill,
};

/// Craten versio build-aikana (`CARGO_PKG_VERSION`).
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Onko koko crate luurankovaiheessa (kaikki putken moduulit pystyssä).
///
/// Väliaikainen totuusarvo joka pitää luuranko-moduulien placeholderit
/// "elossa" (estää `dead_code`-varoitukset CI:n `-D warnings`-portissa)
/// kunnes varsinaiset moduulitoteutukset korvaavat ne.
#[must_use]
pub const fn all_modules_scaffolded() -> bool {
    manifest::SCAFFOLDED
        && registry::SCAFFOLDED
        && policy::SCAFFOLDED
        && approval::SCAFFOLDED
        && audit::SCAFFOLDED
        && dispatch_outbox::SCAFFOLDED
        && task::SCAFFOLDED
        && executor::SCAFFOLDED
        && proof::SCAFFOLDED
        && mcp::SCAFFOLDED
        && pending_store::SCAFFOLDED
        && skills::SCAFFOLDED
        && facade::SCAFFOLDED
        && evals::SCAFFOLDED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nonempty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn scaffold_is_wired() {
        assert!(all_modules_scaffolded());
    }

    #[test]
    fn public_ids_are_reexported() {
        // Jos jokin re-export poistuu, tämä testi ei käänny.
        let _s: SkillId = SkillId::new();
        let _t: ActionTaskId = ActionTaskId::new();
        let _a: ApprovalId = ApprovalId::new();
        let _p: ProofBundleId = ProofBundleId::new();
        let _ac: ActionId = ActionId::new();
        let _e: AuditEventId = AuditEventId::new();
    }

    #[test]
    fn public_error_is_reexported() {
        let err: ActionError = ActionError::UnknownSkill("skill_a".into());
        let res: Result<()> = Err(err);
        assert!(res.is_err());
    }
}
