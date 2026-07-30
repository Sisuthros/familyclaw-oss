# Aethel × FamilyClaw integration

**Status:** design + runnable PoC (this document ships one).
**Scope:** how Aethel policies gate `familyclaw-actions` effects so that an
agent-produced value cannot become a real side effect (tool call, message,
payment, self-modification) unless a policy has verified it first.

---

## 1. The two systems, in one sentence each

- **Aethel** (`<AETHEL_HOME>`, v0.3 alpha) is a deterministic,
  compile-time policy/type DSL. Its one invariant: a `Claim<T>` (untrusted,
  model-produced value) cannot be passed where an effect requires
  `Verified<T, Policy>`. `Verified` has **no public constructor** — it can only
  be minted by `verify(claim, policy)` (Aethel guarantee G5). Effects declare
  the exact verified type they accept
  (`fn execute(a: Verified<UserAction, RiskPolicy>)`), and a function must
  declare `uses EffectName` to call it (G2).
- **FamilyClaw** (this repository, `crates/familyclaw-actions`) is the mature
  ~11.8k-LOC runtime that actually executes work durably and exactly once
  (`observe → plan → approve → execute → verify → proof → remember → report`).

Aethel's own README states the intended stack explicitly:

> *The orchestrating agent OS chooses **why** work is done / Aethel decides **what may be done and
> what proof is required** / FamilyClaw **executes** approved work durably and
> exactly once.*

So they are **complementary, not overlapping**. Aethel is the compile-time
epistemic gate *above* FamilyClaw; FamilyClaw supplies the durable,
exactly-once runtime Aethel deliberately omits (Aethel non-guarantees
NG1/NG2/NG6: no durable resume, `commit_once` is syntactic only, `verify` is a
type-level construct with no runtime authorization).

---

## 2. The gap this closes

FamilyClaw today gates on **risk class + human approval**, and its
cryptographic/postcondition verification runs **after** execution
(`proof.rs` `VerificationResult` / `build_proof` — it *proves what happened*).

What is missing is a gate on the **epistemic provenance of the request payload
before dispatch**. An agent-produced `ActionRequest` is a plain
`serde_json::Value` payload (`executor.rs:65`); nothing in the type system
distinguishes "raw, untrusted, agent-authored" from "checked against a policy".
Aethel's `Claim<T> → Verified<T, Policy>` model is exactly that missing
distinction.

The desired end-state property, expressed as a compile-time guarantee:

> A raw agent request can **never** reach an executor. The only way to obtain
> the token the executor accepts is to pass a policy check.

This is the runtime analogue of Aethel diagnostic **AE-EPISTEMIC-001**.

---

## 3. Concrete mapping to the `familyclaw-actions` API

| Aethel concept | FamilyClaw concrete API | File |
| --- | --- | --- |
| `Claim<ActionRequest>` (untrusted, model-authored) | the JSON `payload` + `input_untrusted` taint flag of `ActionRequest` | `executor.rs:57-73` |
| `verify(claim, Policy)` | `required_approval(risk, policy)` + a consumed, payload-bound `Approval` | `policy.rs:175`, `approval.rs` |
| `Policy` (what evidence is required) | `ActionRisk` × `ApprovalPolicy`; fail-safe: `SpendMoney`/`Irreversible` **always** require approval | `policy.rs:53,126,175` |
| Policy *evidence* (`HumanReview`, `SignedAttestation`) | `Approval` = single-use **nonce** + **SHA-256 payload_hash** + **TTL** | `approval.rs:44,115,175,219` |
| `Verified<ActionRequest, Policy>` | the token that unlocks dispatch (does not exist yet — see §4) | — |
| `effect ActionRuntime.execute(Verified<…>)` | `ActionExecutor::execute(request: ActionRequest)` — the effect boundary | `executor.rs:192-199` |
| `uses ActionRuntime` (effect capability) | `SkillPermission` on the skill manifest | `policy.rs:30-45` |

Risk-class → policy correspondence (from `policy.rs required_approval`):

| `ActionRisk` / `SkillPermission` | Aethel policy strength |
| --- | --- |
| `ReadOnly` | auto-run — models "no `verify` required" |
| `WriteLocal` | auto-run only under `RequireApproval` |
| `SendMessage`, `ExecuteCode`, `WriteExternal` | **must** verify (approval) before dispatch |
| `SpendMoney`, `Irreversible` | **always** verify — fail-safe, policy cannot bypass |

---

## 4. Integration shapes (pick per appetite)

Aethel checks `.aet` **source text**; it is not a Rust crate you call
`aethel::verify()` from at runtime, and its runtime crates are stubs
(`aethel-runtime` 43 LOC, `aethel-store-sqlite` 36 LOC). **Do not route real
effects through Aethel's interpreter/runtime.** There are two viable shapes,
and they compose.

### Shape A — Design gate (ships today, zero code risk) ✅ PoC in this repo

Author one `.aet` spec per high-risk effect, declaring the effect as
`fn(Verified<T, Policy>)`, and run `aethel-cli check` in CI. This machine-checks
the *design claim* "high-risk effects only accept verified inputs" and is a
regression guard — independent of FamilyClaw's dead GitHub-Actions billing
because it runs locally. See `docs/aethel/` (this PoC) and §6.

### Shape B — Port the type-state into Rust (the durable win)

Reimplement Aethel's `Claim<T>` / `Verified<T, P>` type-state at the
`familyclaw-actions` executor boundary, so the guarantee is enforced by
`rustc`, not by a separate tool. This is **additive** and can be staged so it
never breaks the existing build:

**Step 1 (non-breaking, additive module).** Introduce the newtypes with a
private field so `Verified` has no public constructor (ports Aethel G5):

```rust
// crates/familyclaw-actions/src/verified.rs  (new module, additive)
use crate::executor::ActionRequest;
use crate::policy::{ActionRisk, ApprovalPolicy, required_approval, ApprovalRequirement};
use crate::approval::Approval;               // single-use nonce + payload_hash + TTL

/// An untrusted, agent-authored request. Cheap wrapper over ActionRequest.
pub struct Claim<T>(T);
impl<T> Claim<T> {
    pub fn from_agent(value: T) -> Self { Self(value) }
}

/// A request that has passed a policy check. NO public constructor:
/// the only way to obtain one is `verify(...)` below. (Aethel G5.)
pub struct Verified<T, P> { inner: T, _policy: core::marker::PhantomData<P> }
impl<T, P> Verified<T, P> {
    /// Consume the verified value at the effect boundary.
    pub fn into_inner(self) -> T { self.inner }
}

/// Marker type for the "human approval" policy (models HumanApproval.aet).
pub struct HumanApproval;

/// The ONLY minting function — the runtime analogue of `verify(claim, policy)`.
/// Fails closed: no approval evidence, or hash/nonce mismatch => Err.
pub fn verify_with_approval(
    claim: Claim<ActionRequest>,
    risk: ActionRisk,
    policy: ApprovalPolicy,
    approval: &Approval,          // already consumed from ApprovalLedger
) -> crate::Result<Verified<ActionRequest, HumanApproval>> {
    let req = claim.0;
    // Fail-safe path already exists in policy.rs.
    if matches!(required_approval(risk, policy), ApprovalRequirement::RequireApproval) {
        // Evidence check: approval must bind THIS payload (approval.rs payload_hash).
        let hash = crate::approval::sha256_hex(
            serde_json::to_vec(&req.payload).unwrap_or_default().as_slice(),
        );
        if approval.payload_hash != hash {
            return Err(/* ActionError::PolicyDenied */ todo!());
        }
    }
    Ok(Verified { inner: req, _policy: core::marker::PhantomData })
}
```

**Step 2 (the breaking flip, done last).** Change the effect boundary from a
bare payload to the verified token:

```rust
// executor.rs:192-199  — BEFORE
async fn execute(&self, request: ActionRequest) -> Result<ActionResult>;
// AFTER
async fn execute(&self, request: Verified<ActionRequest, HumanApproval>) -> Result<ActionResult>;
```

and have `submit_task` / `submit_task_as` (`facade.rs:849,883`) type the
agent payload as `Claim<ActionRequest>` at entry, calling `verify_with_approval`
using the *existing* `grant_approval`/`ApprovalLedger::consume` evidence
(`approval.rs:175,219`; `facade.rs:1109 approve`) as the mint. Step 2 touches
every `ActionExecutor` impl, so it is the only part that is a real refactor —
stage it behind Step 1 and land it as its own change.

**Recommended order:** ship Shape A now (this PoC), then Shape B Step 1
(additive, safe), then Shape B Step 2 when there's appetite for the trait-churn.

---

## 5. What NOT to do (boundary)

- Do **not** depend on `aethel-runtime`, `aethel-interpreter`, or
  `aethel-store-sqlite` at runtime. They are stubs / a fail-closed symbolic
  simulator (Aethel README + non-guarantees NG1/NG2/NG6/NG8). FamilyClaw's
  `familyclaw-durable` + `dispatch_outbox.rs` remain the exactly-once executor.
- Aethel = the **type/policy gate**. FamilyClaw = the **durable runtime**.
  Integration is a thin compile-time / type-state layer over
  `familyclaw-actions`, never a runtime dependency on Aethel. This division is
  Aethel's own stated design.

---

## 6. PoC (shipped in this repo, runnable)

Files: `docs/aethel/familyclaw_action.aet` (passing) and
`docs/aethel/familyclaw_action_breaker.aet` (breaker). Both model the FamilyClaw
effect boundary: `effect ActionRuntime.execute(Verified<ActionRequest,
HumanApproval>)`, an agent `Claim<ActionRequest>`, and a `HumanApproval` policy
whose evidence mirrors `approval.rs` (payload-bound, single-use, TTL).

```sh
AETHEL_CLI="$AETHEL_HOME/target/release/aethel-cli"   # .exe on Windows

$AETHEL_CLI check docs/aethel/familyclaw_action.aet
# ✓ docs/aethel/familyclaw_action.aet type checks            (exit 0)

$AETHEL_CLI check docs/aethel/familyclaw_action_breaker.aet
# AE-EPISTEMIC-001 at argument 1 to effect `ActionRuntime.execute`:
#   expected `Verified<ActionRequest, HumanApproval>`, found `Claim<ActionRequest>`
#                                                            (exit 1)
```

The passing file is the CI-gate artifact; the breaker file proves the gate
fires when an agent skips `verify()`. Both run entirely on Aethel's mature
front-end — no stub-runtime dependency.

**Verified on 2026-07-26** against Aethel `aethel-cli.exe` (release build): the
passing file exits 0, the breaker file emits AE-EPISTEMIC-001 and exits 1.
