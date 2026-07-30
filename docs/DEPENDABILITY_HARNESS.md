# FamilyClaw Dependability Harness

**Status:** Accepted architecture / implementation in progress
**Branch:** `feat/dependability-harness-v1`
**Owner:** agent_alpha + FamilyClaw
**Scope:** Layer A, model- and provider-agnostic

## 1. Product decision

FamilyClaw is not positioned as another model wrapper or generic multi-agent framework. Its product promise is:

> **FamilyClaw turns replaceable AI models into dependable, explainable, auditable and recoverable work.**

A model response is an observation. It is not proof that the system completed the task. Completion belongs to the harness.

The model may propose, reason and generate. FamilyClaw owns:

- context assembly and compaction,
- retrieval and provenance,
- memory admission and isolation,
- model routing and attempt history,
- tool policy and side-effect boundaries,
- output validation,
- approval and human intervention,
- crash recovery and replay,
- audit correlation,
- evaluation and release gates.

## 2. Non-negotiable invariant

> **No externally visible `completed` state without a Dependability Receipt that passes the active policy.**

`model answered`, `tool returned 200`, `executor reported success`, `workflow stopped` and `process exited 0` are evidence inputs. None is sufficient on its own.

## 3. What already exists

FamilyClaw already has substantial harness primitives:

- durable journal, deterministic replay and at-most-once side-effect machinery,
- action state machine, payload-bound approval and audit events,
- redacted `ProofBundle` and taint propagation,
- contract-checked orchestration and family handoffs,
- Eternal Thread retrieval, provenance gate and session isolation,
- provider failover, key rotation, cooldowns and health snapshots,
- turn audit, resumable approvals and tool-loop limits,
- readiness/canary probes, metrics and traces,
- adversarial action evals and crash/competitor matrices.

The gap is not missing components. The gap is that their evidence is fragmented and no single fail-closed gate owns the meaning of **done**.

## 4. Dependability Receipt

A receipt is a redacted, machine-readable statement of what the harness observed and why the final state was allowed or blocked.

Required top-level fields:

| Field | Meaning |
|---|---|
| `schema_version` | Evolvable receipt contract |
| `subject_id` | Turn, task, workflow or action identifier |
| `trace_id` | Cross-layer correlation identifier |
| `generated_at` | Injected timestamp |
| `status` | Computed `passed` or `blocked`; never caller-declared |
| `checks[]` | Evidence observations by dimension |
| `failures[]` | Machine-readable reasons for a blocked gate |

A receipt must not contain raw prompts, payloads, credentials, private memories or unredacted model/tool output. It stores safe summaries, hashes and stable evidence references.

### Evidence strength

Evidence has ordered strength:

1. `claimed` — model, provider or executor self-report,
2. `structural` — FamilyClaw verified an internal invariant,
3. `independent` — postcondition/read-back was checked independently of the actor that performed the work.

A stronger level satisfies a weaker requirement. A weaker level never satisfies a stronger requirement.

### Dimensions

1. `context` — source manifest, token/size budget, compaction and truncation evidence.
2. `retrieval` — source IDs, relevance, provenance/trust and empty-retrieval reason.
3. `memory` — admission decision, scope/isolation and write/read-back evidence.
4. `model` — attempted routes, failure classes, chosen route and limits.
5. `tool` — contract/schema result, taint, idempotency and side-effect identity.
6. `validation` — task-specific postconditions, schema checks and external read-back.
7. `recovery` — replay/resume state, retry decision and crash boundary.
8. `governance` — policy decision, approval binding and human override.
9. `observability` — trace/audit persistence and receipt discoverability.

### Gate rules

- A required dimension with no evidence blocks.
- A required dimension below its minimum evidence strength blocks.
- Any explicit failed check blocks, even if that dimension was not otherwise required.
- `not_applicable` is explicit and auditable; it never satisfies a requirement.
- Gate status is computed from checks + policy and cannot be supplied by a model, tool or caller.
- Unknown states fail closed.

## 5. Runtime architecture

```text
Input
  │
  ▼
Context manifest ── Retrieval provenance ── Memory scope
  │
  ▼
Model attempt ledger ── Tool/contract execution ── Human gate
  │
  ▼
Independent postconditions ── Recovery evidence ── Audit persistence
  │
  ▼
Dependability Gate ──► PASSED / BLOCKED
  │
  └──► redacted Dependability Receipt
```

### Crate boundary

`familyclaw-harness` is a dependency-light Layer A crate. It owns:

- the neutral receipt schema,
- evidence-strength ordering,
- policy requirements,
- deterministic fail-closed evaluation.

It does not call models, tools, storage, networks or operators. Existing crates produce adapters/evidence:

- `familyclaw-agent`: context, retrieval, model attempts, turn stop state,
- `familyclaw-actions`: policy, execution, validation, proof and approval,
- `familyclaw-durable`: replay/commit/recovery evidence,
- `familyclaw-memory`: provenance and admission evidence,
- `familyclaw-observability`: trace persistence and metrics,
- `familyclaw-gateway`: API/console exposure and release readiness.

This direction avoids dependency cycles and prevents the harness from becoming another orchestrator.

## 6. Policy profiles

### Informational turn

Minimum requirements:

- context: `structural`,
- model: `structural`,
- validation: `claimed` initially, upgraded by task class,
- observability: `structural`.

### Tool-assisted turn

Adds:

- tool: `structural`,
- governance: `structural` when a capability has side effects,
- recovery: `structural` when a retry, suspend or replay path is entered.

### External side effect

Minimum requirements:

- tool: `structural`,
- validation: `independent`,
- governance: `structural`,
- recovery: `structural`,
- observability: `structural`.

A tool's own `success` flag is only `claimed` evidence and cannot satisfy independent validation.

## 7. SLOs and error budget

These are release gates, not dashboard aspirations:

| SLO | Target |
|---|---:|
| Externally visible completed tasks with a passing receipt | 100% |
| Known failed check released as completed | 0 |
| Approval bypass or payload-binding mismatch | 0 |
| Duplicate external side effects after replay/retry | 0 |
| Suspended approvals with non-durable resume state | 0 |
| Tool/action receipts with trace correlation | 100% |
| Retrieval evidence with provenance and source identity | 100% when retrieval is used |
| Memory writes with admission + scope decision | 100% |
| Critical postconditions checked independently | 100% |
| Tampered receipt/audit accepted as valid | 0 |

Latency and availability SLOs remain measurement-only until a production baseline exists. They must not be invented to make a scorecard green.

## 8. Dependability Scorecard

The release scorecard combines existing continuity/security evidence with production gates. `overall = PASS` only when every hard dimension passes:

| ID | Dimension | Hard threshold |
|---|---|---|
| D1 | Crash and at-most-once dispatch | duplicate external side effects `= 0`; resume and baseline match `= 1.0`; corruption fails loudly |
| D2 | Tool safety and containment | sandbox escapes, unapproved executions and policy bypasses `= 0` |
| D3 | Memory integrity | false merges/admissions `= 0`; protected core and poison blocking `= 1.0` |
| D4 | Retrieval and context | required provenance present; unverified action claims excluded; poison/stale-context suites green |
| D5 | Claim grounding | externally visible “done/wrote/sent/refunded” claims without bound proof `= 0` |
| D6 | Recovery and failover | replay overcount `= 0`; intent-only never re-fires; bounded provider failover passes |
| D7 | Human escalation | high-risk execution without valid approval `= 0`; expiry/rejection/crash-resume remain fail-closed |
| D8 | Family co-failure | external work is cross-verified; family and pairwise co-failure rates are measured before becoming release thresholds |

Planned machine-readable artifacts:

- `docs/DEPENDABILITY_SCORECARD.md`,
- `crates/familyclaw-bench/out/dependability_scorecard.json`,
- `cargo run -p familyclaw-bench -- dependability`, exiting non-zero on any hard failure.

The product claim is **at-most-once dispatch**, not magical exactly-once completion of arbitrary external work. Intent-only means uncertain and recovery-required—not success and not safe automatic retry.

## 9. Adversarial evaluation matrix

Every release must cover at least:

1. model returns fluent but false success,
2. model returns malformed tool arguments,
3. tool returns success but postcondition read-back fails,
4. stale or low-provenance retrieval dominates context,
5. context compaction drops a required invariant,
6. provider timeout/rate-limit/auth failure triggers bounded failover,
7. crash before side effect,
8. crash after side effect but before local commit,
9. approval payload changes after grant,
10. approval suspend cannot be persisted,
11. audit/trace sink unavailable,
12. unknown evidence state or receipt schema,
13. memory poisoning attempt,
14. co-failing family agents produce correlated false confidence,
15. human rejection/expiry/override survives restart,
16. refund/teardown/migration pack scenarios produce side-effect overcount `= 0`,
17. fabricated `done`, path, message or money claims without dispatch/proof are blocked,
18. approval survives process death and resumes to exactly one dispatch,
19. gateway kill/restart preserves pending/outbox truth without duplicate dispatch.

Each case must assert both positive behavior and the **absence** of forbidden side effects or false completion.

## 10. Implementation slices

### P0 — Ground truth (**implemented on this branch**)

- canonical architecture and policy,
- neutral receipt/gate crate,
- tests proving invalid identity plus missing, weak and failed evidence block,
- approval consumption order is fixed: pending check + resumable write share the `ActionRuntime` lock, and chat approval loads durable continuation state before consuming the single-use approval,
- regression: non-durable approval suspend fails closed; rollback writes a dispatch-outbox quarantine before pending cleanup, so even a tombstone failure cannot execute the orphan approval.

### P1 — Action adapter (**baseline implemented; enforcement pending**)

Implemented:

- convert completed `PipelineOutcome`, `ProofBundle` and audit IDs into receipt checks,
- distinguish executor status claim from independent postcondition validation,
- read-only compatibility profile passes with explicitly `claimed` validation,
- critical profile blocks the same outcome until independent validation, governance and recovery evidence exist.

Remaining:

- derive the active profile from the trusted skill manifest/risk class,
- add skill-specific independent postcondition/read-back validators,
- prevent `TaskStatus::Done` until the derived action policy passes.

### P2 — Turn adapter

- context manifest with compaction/truncation data,
- retrieval result IDs/provenance without raw memory content,
- model-attempt ledger correlated to turn trace,
- turn `answered` separated from `completed`.

### P3 — Recovery and persistence

- durable receipt journal with integrity chain,
- receipt replay/read-back,
- attach existing fail-closed suspend/resume outcomes and quarantine evidence to recovery receipts,
- crash-boundary evals emit receipts.

### P4 — Operator surface

Ship the smallest truthful surface first: `GET /dependability/snapshot` plus one Reliability Console card containing:

- durability modes (`journal` or `in-memory`) and crash-survival truth,
- canonical `now` state rather than only an SSE tail,
- pending approvals with redacted summary and TTL,
- `intent_only`/uncertain dispatches with policy `fail_closed_no_rerun`,
- the next safe operator action and recent redacted audit events.

There is no Retry button for an uncertain dispatch. The operator must inspect the external system before an explicit later resolution protocol. Approve means **“agent resumes”**, never **“side effect completed.”** Raw payloads, prompts, credentials and private memories remain absent.

After the snapshot: `GET /dependability/receipts/:subject_id` and a correlated run timeline.

### P5 — Release gate and product package

- `familyclaw dependability check`,
- D1–D8 scorecard JSON + Markdown,
- automated pack overcount, false-claim, approval-crash and gateway-kill scenarios,
- CI regression corpus,
- signed/exportable customer evidence pack,
- documented policy profiles and extension SPI.

## 11. Acceptance gate for v1

v1 is not complete until:

- gate behavior is deterministic and serialization-stable,
- missing/weak/failed evidence is proven fail-closed,
- at least one real action and one real turn emit receipts,
- `Done`/`completed` cannot bypass the gate,
- suspend persistence failure cannot masquerade as recoverable,
- receipt data is redacted and correlated to existing audit/proof IDs,
- D1–D7 hard gates are machine-readable and fail the release command,
- the operator snapshot exposes uncertain dispatch without offering automatic retry,
- focused, crate and workspace tests pass,
- one live demo shows the same task with two different models producing the same harness verdict.

## 12. Commercial message

**Models provide intelligence. FamilyClaw provides operational truth.**

The sellable outcome is not access to a particular model. It is evidence that work was executed under explicit context, policy, verification and recovery guarantees—and that the system refused to claim completion when those guarantees were absent.
