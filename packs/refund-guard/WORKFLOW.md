# refund-guard — production seam

1. **Intent:** agent proposes `refund.create` with amount + customer id.
2. **Approval:** skill declares `AlwaysRequireApproval` / `RequireApproval`;
   operator sees a redacted summary in `/console` (or Slack).
3. **Dispatch:** after approval, `submit_task_idempotent` records intent →
   calls PSP → commits outbox. Crash between intent and commit fails closed.
4. **Idempotency key:** `hash(approval_id || amount || currency || customer)`.
   The PSP must also treat that key as at-most-once.

Do not put PSP secrets in this pack — load them via `FAMILYCLAW_PROFILE_DIR`.
