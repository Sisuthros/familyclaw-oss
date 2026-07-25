# infra-teardown — production seam

1. Agent proposes resource delete (ARN / project id).
2. Skill risk = `WriteExternal` + `AlwaysRequireApproval`.
3. Operator reviews redacted summary in `/console`.
4. Dispatch uses idempotency key = `hash(resource_id || action)`.
5. Optional: Time Machine fork + dry-run before enabling live credentials.
