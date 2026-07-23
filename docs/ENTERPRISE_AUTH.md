# Enterprise auth — gateway token, OIDC, operator RBAC

FamilyClaw is a **single-tenant, self-hosted reliability appliance**. Auth is
designed fail-closed for anything that can inject work or approve side effects.

## 1. Gateway bearer token (required off-loopback)

| Bind | Token empty | Behavior |
|---|---|---|
| `127.0.0.1` / `::1` | allowed | Open local eval; `doctor` warns |
| `0.0.0.0` / non-loopback | **rejected** | `serve` and `doctor` fail closed |

Env: `FAMILYCLAW_GATEWAY_TOKEN`. Protects `/inject`, `/approvals/*`,
`/turns/audit`, task enable routes. `/healthz`, `/readyz`, `/metrics` stay
probe-open (no secrets).

Docker Compose **requires** the token (see root `docker-compose.yml`).

## 2. OIDC / SSO (supported pattern)

Native IdP protocol negotiation is intentionally **not** embedded in the
gateway binary (keeps the dependency surface small). Production SSO:

1. Put **oauth2-proxy** / Entra / Okta reverse-proxy in front of `:8787`.
2. Proxy authenticates operators (OIDC) and injects
   `Authorization: Bearer <FAMILYCLAW_GATEWAY_TOKEN>` toward the gateway
   (or only exposes the port on a private network and uses mTLS).
3. Optionally set operator role headers the gateway ACL understands
   (below).

Env knobs reserved for future native OIDC (half-config is ignored safely):

| Env | Purpose |
|---|---|
| `FAMILYCLAW_OIDC_ISSUER` | IdP issuer URL (documentation / future) |
| `FAMILYCLAW_OIDC_AUDIENCE` | Expected audience |
| `FAMILYCLAW_OIDC_JWKS_URL` | JWKS endpoint |

Until native validation ships, treat these as **integration contract** fields
for your reverse proxy config — do not leave the gateway on `0.0.0.0`
without a token.

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
bearer token. Missing/invalid role → `403` when ACL is enabled.

## 4. Positioning

This is **not** multi-tenant SaaS IAM. Org isolation = separate appliance
(or separate `FAMILYCLAW_DATA_DIR` + token) per tenant.
