# Enterprise auth — gateway token, OIDC, operator RBAC

FamilyClaw is a **single-tenant, self-hosted reliability appliance**. Auth is
designed fail-closed for anything that can inject work or approve side effects.

## 1. Gateway bearer token (required off-loopback)

| Bind | Token empty | Behavior |
|---|---|---|
| `127.0.0.1` / `::1` | allowed | Open local eval; `doctor` warns |
| `0.0.0.0` / non-loopback | **rejected** | `serve` and `doctor` fail closed |

Env: `FAMILYCLAW_GATEWAY_TOKEN`. Protects `/inject`, `/approvals/*`,
`/turns/audit`, `/console/events`, task enable routes. `/healthz`, `/readyz`,
`/metrics`, and the `/console` HTML shell stay probe-open (no secrets in the
shell; the SSE stream still requires auth when a token/OIDC is configured).

Docker Compose **requires** the token (see root `docker-compose.yml`).

## 2. Native OIDC / JWT (optional)

When fully configured, protected routes accept **either** the static gateway
token **or** a Bearer JWT whose `iss` / `aud` / `exp` match the IdP.

Half-configuration **fails closed at `serve` startup** (any OIDC env set
without a complete set → error).

| Env | Purpose |
|---|---|
| `FAMILYCLAW_OIDC_ISSUER` | Expected `iss` |
| `FAMILYCLAW_OIDC_AUDIENCE` | Expected `aud` |
| `FAMILYCLAW_OIDC_JWKS_URL` | JWKS endpoint (RSA/EC) |
| `FAMILYCLAW_OIDC_HS256_SECRET` | Shared-secret HS256 (tests / simple IdPs) |

Complete set = issuer + audience + (`JWKS_URL` **or** `HS256_SECRET`).

Reverse-proxy SSO (oauth2-proxy / Entra / Okta in front of `:8787`) remains a
supported pattern: the proxy can inject `Authorization: Bearer
<FAMILYCLAW_GATEWAY_TOKEN>` toward the gateway. Native JWT validation is for
operators who want the appliance to verify IdP tokens directly.

## 3. Operator RBAC (approvals)

Capability identifiers (deny-by-default), used with
`familyclaw_observability::RbacPolicy` / `OperatorAcl`:

| Capability | Meaning |
|---|---|
| `approvals.read` | List pending approvals |
| `approvals.decide` | Approve / deny |
| `audit.read` | Read turn audit |
| `tasks.control` | Enable/disable scheduled tasks |

Default production ACL (when `FAMILYCLAW_OPERATOR_ACL=1`):

- Role `viewer` → `approvals.read`, `audit.read`
- Role `approver` → viewer + `approvals.decide`
- Role `admin` → all

Pass role via `X-FamilyClaw-Operator-Role: approver` **in addition to** the
bearer token (or OIDC JWT). Missing/invalid role → `403` when ACL is enabled.

## 4. Durable journal backends

| Backend | Feature / env | Notes |
|---|---|---|
| `FileJournal` | default | Append-only JSONL + fsync — **the only backend `serve` uses** |
| `PostgresJournal` | `familyclaw-durable/postgres`, `DATABASE_URL` | Same `Journal` contract; single-tenant. **Library only** — see below |

`PostgresJournal` is not yet selectable at runtime: nothing in the gateway or
the agent runtime constructs it, so `serve` opens a `FileJournal` even when
`DATABASE_URL` is set. Today it is usable only by embedding `familyclaw-durable`
in your own binary. Wiring a `FAMILYCLAW_JOURNAL_BACKEND`-style switch is open
work.

## 5. Trace export (OTLP scaffolding)

| Env | Purpose |
|---|---|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Base URL for a collector adapter |

With `familyclaw-observability` feature `otlp`, `TraceContext` can be wrapped
as an `OtlpSpanEnvelope` JSON payload for `/v1/traces`. This is **not** a full
OpenTelemetry SDK — it is the RFP-facing export hook.

## 6. Positioning

This is **not** multi-tenant SaaS IAM. Org isolation = separate appliance
(or separate `FAMILYCLAW_DATA_DIR` + token) per tenant.
