#!/usr/bin/env bash
# FamilyClaw — canonical crash proof (one command).
# ============================================================
# Kill the agent. Restart it. Count the side effects.
#
# This script is the single reproducible proof behind FamilyClaw's release
# claim: **at-most-once external dispatch across the tested crash and replay
# windows**. It needs no API keys and no network — the "external effect" is a
# deterministic on-disk sink (a counter file) written by an approval-gated
# skill classified `WriteExternal`.
#
# It drives the `dispatch_redteam` black box (crates/familyclaw-actions), the
# same harness the committed integration tests use, but across a real process
# boundary: the crashing process exits 137 (SIGKILL-style) inside the window
# where the side effect has already fired but the durable commit record has
# not. A second, fresh process then replays durable state.
#
# Two windows are proven, both on the APPROVAL path (approval is bound to a
# durable pending record; the fresh process reloads the SAME ApprovalId from
# disk before re-approving):
#
#   1. INTENT-ONLY window (the dangerous one) — crash after `record_intent`
#      and after the side effect, before `record_committed`. Replay must be
#      fail-closed (`PolicyDenied`) and must NOT re-run the side effect.
#   2. COMMITTED window — crash after `record_committed`, before the pending
#      record is removed. Replay must return the value-identical outcome and
#      must NOT re-run the side effect.
#
# Usage:
#   bash scripts/crash-proof.sh
#
# Exit code is 0 only if every invariant holds; any violation exits non-zero.

set -u

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT" || exit 2

# Deterministic injected clock — the harness takes the wall clock as input so
# the whole proof is reproducible.
CLOCK="2026-01-01T00:00:00Z"
FAIL=0

note()  { printf '%s\n' "$*"; }
fail()  { printf 'FAIL: %s\n' "$*"; FAIL=1; }

note "═══════════════════════════════════════════════════════════"
note "  FamilyClaw — crash proof (at-most-once external dispatch)"
note "═══════════════════════════════════════════════════════════"
note ""

# ── Build the black box ────────────────────────────────────────────────────
note ">>> Building dispatch_redteam ..."
if ! cargo build -q -p familyclaw-actions --bin dispatch_redteam 2>&1; then
    fail "cargo build -p familyclaw-actions --bin dispatch_redteam"
    printf 'overall = FAIL\n'
    exit 2
fi

BIN="$REPO_ROOT/target/debug/dispatch_redteam"
[ -x "$BIN" ] || BIN="$REPO_ROOT/target/debug/dispatch_redteam.exe"
if [ ! -x "$BIN" ]; then
    fail "dispatch_redteam binary not found under target/debug/"
    printf 'overall = FAIL\n'
    exit 2
fi
note "    binary: $BIN"
note ""

# ── Helpers ────────────────────────────────────────────────────────────────

# Extracts the `RESULT <json>` line the harness prints on stdout.
result_line() { sed -n 's/^RESULT //p' "$1" | tail -n1; }

# Reads a scalar field out of the RESULT json without needing jq.
json_field() {
    python -c "
import json,sys
try:
    d=json.loads(sys.argv[1])
except Exception:
    print('__PARSE_ERROR__'); sys.exit(0)
v=d.get(sys.argv[2])
print('' if v is None else (str(v).lower() if isinstance(v,bool) else str(v)))
" "$1" "$2" 2>/dev/null
}

read_counter() {
    if [ -f "$1" ]; then tr -dc '0-9' < "$1"; else printf '0'; fi
}

# Runs one crash window end to end.
#   $1 = human label, $2 = crash phase, $3 = crash-arming env var,
#   $4 = expectation: "denied" (intent-only) or "identical" (committed)
# Sets the global WINDOW_COUNT to the observed side-effect count (this must NOT
# be returned via stdout — the function also prints its human-readable log
# there, so command substitution would capture the whole transcript).
# Sets FAIL on any violated invariant.
WINDOW_COUNT=0
run_window() {
    local label="$1" crash_phase="$2" arm_env="$3" expect="$4"
    local state; state="$(mktemp -d)"
    local outbox="$state/outbox" counter="$state/counter" outcome="$state/outcome"
    local pending="$state/pending" queue="$state/queue"
    local crash_out="$state/crash.out" resume_out="$state/resume.out"

    note "─── $label ───"
    note "    state dir (clean durable state): $state"

    # Phase 1: dispatch + approve, then die inside the window.
    env "$arm_env=1" "$BIN" run \
        --mode new --phase "$crash_phase" \
        --outbox "$outbox" --counter "$counter" --outcome-out "$outcome" \
        --pending "$pending" --task-queue "$queue" \
        --clock "$CLOCK" > "$crash_out" 2>&1
    local crash_code=$?
    local after_crash; after_crash="$(read_counter "$counter")"
    note "    crash phase   : $crash_phase -> exit $crash_code (expect 137)"
    note "    side effects  : $after_crash (expect 1 — the effect DID happen)"
    [ "$crash_code" = "137" ] || fail "$label: crash phase exited $crash_code, expected 137 (SIGKILL-style)"
    [ "$after_crash" = "1" ]  || fail "$label: side effect count after crash = $after_crash, expected 1"

    # Phase 2: a FRESH process replays durable state.
    "$BIN" run \
        --mode new --phase approve_resume \
        --outbox "$outbox" --counter "$counter" --outcome-out "$outcome" \
        --pending "$pending" --task-queue "$queue" \
        --clock "$CLOCK" > "$resume_out" 2>&1
    local resume_code=$?
    local rjson; rjson="$(result_line "$resume_out")"
    if [ -z "$rjson" ]; then
        fail "$label: resume phase printed no RESULT line (exit $resume_code)"
        sed 's/^/      | /' "$resume_out" | head -20
        WINDOW_COUNT="$after_crash"
        return
    fi

    local final denied identical reloaded
    final="$(json_field "$rjson" side_effect_count)"
    denied="$(json_field "$rjson" policy_denied)"
    identical="$(json_field "$rjson" value_identical)"
    reloaded="$(json_field "$rjson" reloaded_approval_id)"

    note "    resume phase  : approve_resume -> exit $resume_code"
    note "    side effects  : $final (expect 1 — replay must NOT re-fire)"
    note "    approval id   : $reloaded (reloaded from durable pending surface)"
    note "    policy_denied : $denied   value_identical: $identical"

    [ "$final" = "1" ] || fail "$label: side effect count after replay = $final, expected 1 (DUPLICATE DISPATCH)"
    [ -n "$reloaded" ] || fail "$label: fresh process did not reload an ApprovalId from durable state"

    case "$expect" in
        denied)
            [ "$denied" = "true" ] || fail "$label: replay was not fail-closed (policy_denied=$denied)"
            ;;
        identical)
            [ "$identical" = "true" ] || fail "$label: replayed outcome not value-identical (value_identical=$identical)"
            ;;
    esac

    note ""
    WINDOW_COUNT="$final"
}

# ── Window 1: INTENT-ONLY (the dangerous window) ───────────────────────────
run_window \
    "Window 1/2 — INTENT-ONLY crash (effect fired, commit record missing)" \
    approve_crash_intent \
    FAMILYCLAW_REDTEAM_CRASH_AFTER_INTENT \
    denied
C1="$WINDOW_COUNT"

# ── Window 2: COMMITTED ────────────────────────────────────────────────────
run_window \
    "Window 2/2 — COMMITTED crash (commit record on disk, cleanup missing)" \
    approve_crash_committed \
    FAMILYCLAW_REDTEAM_CRASH_AFTER_COMMITTED \
    identical
C2="$WINDOW_COUNT"

# ── Verdict ────────────────────────────────────────────────────────────────
C1="${C1:-0}"
C2="${C2:-0}"
OVER1=$(( C1 - 1 ))
OVER2=$(( C2 - 1 ))
OVERCOUNT=$(( OVER1 > OVER2 ? OVER1 : OVER2 ))
[ "$OVERCOUNT" -lt 0 ] && OVERCOUNT=0

if [ "$FAIL" = "0" ]; then
    APPROVAL_MATCH="PASS"
else
    APPROVAL_MATCH="FAIL"
fi

# Proof receipt: a stable digest over the exact inputs and observations, so a
# reported run can be tied back to what was actually measured.
RECEIPT_INPUT="familyclaw-crash-proof/v1
commit=$(git rev-parse HEAD 2>/dev/null || echo unknown)
rustc=$(rustc --version 2>/dev/null)
clock=$CLOCK
window1_intent_only_side_effects=$C1
window2_committed_side_effects=$C2
overcount=$OVERCOUNT
fail=$FAIL"
RECEIPT="$(printf '%s' "$RECEIPT_INPUT" | sha256sum 2>/dev/null | cut -c1-16)"
[ -n "$RECEIPT" ] || RECEIPT="unavailable"

note "═══════════════════════════════════════════════════════════"
note "  VERDICT"
note "═══════════════════════════════════════════════════════════"
printf 'side_effect_overcount = %s\n' "$OVERCOUNT"
printf 'approval_payload_match = %s\n' "$APPROVAL_MATCH"
printf 'proof_receipt = %s\n' "$RECEIPT"
if [ "$FAIL" = "0" ] && [ "$OVERCOUNT" = "0" ]; then
    printf 'overall = PASS\n'
    exit 0
fi
printf 'overall = FAIL\n'
exit 1
