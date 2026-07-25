# migration-runner — production seam

Treat each migration step as an external side effect with an idempotency key
(`migration_id || step_index`). FamilyClaw's BeforeWrite crash point is the
gap after the SQL ran but before the journal row committed — the outbox closes
that gap. MidReplay proves resume itself is crash-safe.
