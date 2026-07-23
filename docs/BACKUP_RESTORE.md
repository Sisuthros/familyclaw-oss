# Backup and restore — durable journal + data dir

FamilyClaw's crash guarantees depend on the on-disk journal under
`FAMILYCLAW_DATA_DIR`. This runbook is the **single-tenant appliance**
backup path (no multi-node HA claim).

## What to back up

| Path | Contents |
|---|---|
| `$FAMILYCLAW_DATA_DIR/` | Durable journal, memory stores, approval state |
| `$FAMILYCLAW_PROFILE_DIR/` | Layer B souls/keys (**never** commit; encrypt at rest) |

Do **not** back up process memory or `/metrics` scrapes — they are ephemeral.

## Cold backup (preferred)

1. Stop the gateway cleanly (`Ctrl-C` or `docker compose stop gateway`).
2. Archive the data dir:

```bash
tar -C "$FAMILYCLAW_DATA_DIR" -czf "familyclaw-data-$(date -u +%Y%m%dT%H%M%SZ).tar.gz" .
```

3. Store the archive off-box. Verify checksum.

## Restore

1. Stop the gateway.
2. Replace `$FAMILYCLAW_DATA_DIR` with the archive contents (empty target first).
3. Start the gateway with the **same** `FAMILYCLAW_DATA_DIR`.
4. `familyclaw-gateway doctor` and `GET /readyz` must succeed.
5. Optionally run `cargo run -p familyclaw-agent --bin crash_replay -- full` on a
   copy of the journal in a scratch dir to confirm replay still works.

## Hash-chained audit export (SIEM)

Copy the journal files and record:

```text
sha256sum $FAMILYCLAW_DATA_DIR/**  > audit-export.sha256
```

Ship `audit-export.sha256` + the tarball to your SIEM / evidence locker.
Journal records are append-oriented; tampering changes hashes. See
`docs/SECURITY_MODEL.md` Layer 7 and `familyclaw-durable` Time Machine for
inspect/fork/diff without re-dispatch.

## Kill / restart verification

Use [`scripts/docker-kill-restart-verify.ps1`](../scripts/docker-kill-restart-verify.ps1)
(or `.sh`) after `docker compose up`. Expected: `/healthz` returns after
SIGKILL + restart with the same volume; no token → container must refuse to
start on `0.0.0.0`.
