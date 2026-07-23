# Observability — metrics, audit, tracing

## Metrics

`GET /metrics` — Prometheus text from `MetricsRegistry::prometheus_export`
(deterministic series order). Unauthenticated on purpose (numeric only).

## Turn audit

`GET /turns/audit` — redacted tool-loop events. Requires gateway bearer when
configured (mandatory off-loopback).

## Structured correlation (lightweight tracing)

FamilyClaw stays dependency-light (no full OpenTelemetry SDK in the default
binary). Operators get W3C-compatible correlation via:

| Field | Source |
|---|---|
| `trace_id` | `familyclaw_observability::TraceContext::new()` / continue from `traceparent` |
| `turn` | Agent turn counter |
| `dispatch_id` | Idempotency key in the actions outbox |

Log line shape (English):

```text
turn-provider: turn=42 model=… failovers=1 final_error_class=… trace_id=…
```

Export path for SIEM: journal tarball + `sha256sum` manifest
(`docs/BACKUP_RESTORE.md`).

## Future OTLP

An optional `otlp` feature may export spans later. Until then, scrape
Prometheus + ship audit JSON / journal hashes.
