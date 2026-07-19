<!--
  FamilyClaw vs NVIDIA NemoClaw — sales and positioning comparison.
  Fact-based: references NVIDIA's own alpha-status statement + third-party
  pentests (Lasso Security, natoma.ai). Does NOT claim anything NVIDIA has
  not admitted itself. Prepared 2026-07-09.
-->

# FamilyClaw vs NVIDIA NemoClaw + OpenShell

**One-sentence difference:** NemoClaw isolates *where* an agent can go (kernel
level: seccomp + Landlock + network namespaces). FamilyClaw additionally
governs *what the agent tries to do* within the allowed channel (agent level:
taint tracing, tool approval, at-most-once dispatch).

> This isn't "NemoClaw is bad". NVIDIA's release **validates the market** — a
> major player said out loud that securing always-on agents is a real problem.
> FamilyClaw solves the same problem more deeply and is production-ready.

---

## Starting point: what NemoClaw is

- **A security *wrapper*, not its own runtime.** NemoClaw runs existing agents
  (OpenClaw default, Hermes, LangChain Deep Agents) inside an OpenShell sandbox.
- **NVIDIA's own status: alpha / early-preview** (released 2026-03-16),
  explicitly *"not production-ready"*, "APIs and behavior may change without
  notice".
- Apache-2.0, free. Strong mindshare (NVIDIA brand).

---

## Proven gaps (external sources, not our claims)

### 1. Lasso Security pentest: allowed binaries leak data
OpenShell's egress policy is *"correctly enforced but doesn't evaluate
intent"*. Three proven exfiltration paths, ALL of which use the sandbox's
**required/allowed** binaries:
1. `gh` → creates a PR whose body carries data out via the GitHub API
2. `npm` postinstall scripts → code execution during install
3. `node` runtime → data sent out via a Discord integration

Target: `/sandbox/.openclaw/openclaw.json` (credentials + API keys in
plaintext) + env variables. Lasso's conclusion: *"the sandbox is not a silver
bullet if the agent inside is structurally forced to interact with the
outside world"* — channels are mutually substitutable (harden Discord, the
attacker switches to gh).

### 2. natoma.ai: no tool/MCP-level authorization
*"An open network path is not governed access. The agent can reach Slack.
Nothing controls what it does once it gets there."* NemoClaw covers only
compute isolation (network + filesystem + processes), NOT tool-level
governance. The audit trail shows only the frequency of network connections,
not the actual operations: *"it doesn't show that 'it posted 12 standup
summaries, commented on 8 PRs'."* — *"network egress rules can't govern tool
selection."*

---

## How FamilyClaw's layers prevent these same attacks

| NemoClaw gap (proven) | FamilyClaw's response (architectural) |
|---|---|
| **Allowed binaries (node/npm/git/gh) leak data** (Lasso) | **Layer 6 (Wasmtime sandbox):** 3rd-party code runs as WASM bytecode with fuel caps + capability gating — native binaries have NO access by default. The entire binary-misuse class disappears *structurally*, not at the policy level. |
| **npm postinstall → credential exfil** (Lasso) | **Layer 2 (Fail-closed approvals) + Layer 3 (Taint tracing):** external content is taint-marked; an action that moves tainted data out requires explicit approval (fail-closed). |
| **Silent SOUL.md / config edits** | **Layer 5 (Identity-anchor tamper alert):** a change to the identity file triggers an alert. |
| **No tool/MCP authorization** ("nothing controls what it does") (natoma.ai) | **Layer 1 (Allowlist roots) + tool-policy separation** (sandbox vs tool-policy vs elevated): FamilyClaw governs WHAT the agent does WITHIN the allowed channel, not just where it connects. |
| **Audit trail shows only network connections** (natoma.ai) | **Hash-chained journal + at-most-once dispatch (Layer 7):** every operation is recorded tamper-evidently; SIGKILL-tested to prevent double dispatch. |
| **Credentials in plaintext in openclaw.json** (Lasso) | **Layer 4 (Redaction) + env-scrub:** keys live in env vars, redacted from logs/journal; the sandbox seed never contains keys. |

---

## Maturity gap

| | NemoClaw + OpenShell | FamilyClaw |
|---|---|---|
| **Status** | alpha / early-preview (NVIDIA's own words) | 1809 tests green, CI gates (fmt/clippy/scorecard) |
| **"Breaking changes without notice"** | yes (documented) | stable API, versioned crates |
| **Security model** | kernel isolation (Linux container) | 8 layers: isolation + agent-internal semantic security |
| **3rd-party code** | full native binaries in sandbox | WASM-only (fuel + capability) |
| **Tool/MCP authorization** | no (natoma.ai) | Layer 1 + tool-policy |

---

## Sales pitch (AI Expo, Cyprus)

> **"We're not claiming anything NVIDIA hasn't admitted itself.** NemoClaw is
> valuable — it proves that agent security is a real, big problem. But by
> NVIDIA's own words it is *not production-ready*, and an independent pentest
> (Lasso Security) showed that its sandbox leaked credentials via three
> separate paths. FamilyClaw fixes exactly those attack classes
> *architecturally* — WASM eliminates binary misuse entirely, and we govern
> what the agent does, not just where it connects. 1809 tests, production-ready,
> today."**

**Honest caveat:** alpha status is a time window, not a permanent advantage —
NVIDIA can close the gaps. Use it now. And: before FamilyClaw markets itself
as "more secure," an independent pentest of its own would strengthen the claim
(Lasso tested NemoClaw, not FamilyClaw) — but the layer mapping above is
verifiable from the source code today.

---

## Sources
- NVIDIA NemoClaw: https://github.com/NVIDIA/NemoClaw · docs.nvidia.com/nemoclaw
- Lasso Security pentest (OpenShell sandbox escape / data exfiltration)
- natoma.ai analysis (lack of tool/MCP authorization)
- FamilyClaw layers: `docs/SECURITY_MODEL.md` (Layer 1-8, verifiable)
