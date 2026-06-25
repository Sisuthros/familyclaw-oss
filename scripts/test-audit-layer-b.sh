#!/usr/bin/env bash
# Regression tests for scripts/audit-layer-b.sh
# ============================================================
# Proves the Layer B audit scans ALL tracked TEXT files (content-based), not an
# extension allowlist, so a leaked private name in any text format FAILS the
# audit. Each case builds a throwaway git sandbox (temp dir, `git init`, add a
# file, run a COPY of the audit against it) so the real repo is never polluted.
#
# ── Why no real private name appears in THIS file ──────────────────────────
# The committed test must prove the scanner catches a forbidden private name,
# but if it embedded a real family name in plaintext the repo would fail its
# OWN audit (check #8 scans this very file). Solution: the forbidden fixture
# string is CONSTRUCTED AT RUNTIME from the audit script's own FORBIDDEN_NAMES
# definition (see `forbidden_name` below). No real name is ever committed here.

set -u

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AUDIT_SCRIPT="$REPO_ROOT/scripts/audit-layer-b.sh"

if [ ! -f "$AUDIT_SCRIPT" ]; then
    echo "FATAL: audit script not found at $AUDIT_SCRIPT" >&2
    exit 2
fi

PASS=0
FAIL=0

# Derive a real forbidden private name from the audit's OWN list at runtime, so
# this test file never contains one in plaintext. Pull the first token of the
# FORBIDDEN_NAMES="..." assignment and strip any quoting.
forbidden_name() {
    grep -E '^FORBIDDEN_NAMES=' "$AUDIT_SCRIPT" \
        | head -n1 \
        | sed -E 's/^FORBIDDEN_NAMES="?//; s/".*$//' \
        | tr -d '"' \
        | awk '{print $1}'
}

# Build an isolated git sandbox containing a copy of the audit script (so its
# self-exclusion path `scripts/audit-layer-b.sh` and CWD-relative `git ls-files`
# behave exactly as in the real repo), commit a caller-provided fixture, then run
# the audit with the sandbox as CWD. Echoes the audit exit code.
run_audit_with_fixture() {
    # $1 = relative fixture path, $2 = fixture content (text), $3 = "binary" flag (optional)
    local rel="$1"
    local content="$2"
    local mode="${3:-text}"
    local sandbox
    sandbox="$(mktemp -d)"

    (
        cd "$sandbox" || exit 99
        git init -q
        git config user.email "test@example.com"
        git config user.name "audit-test"
        mkdir -p scripts
        cp "$AUDIT_SCRIPT" scripts/audit-layer-b.sh

        mkdir -p "$(dirname "$rel")" 2>/dev/null || true
        if [ "$mode" = "binary" ]; then
            # NUL bytes → grep -I classifies as binary → audit must skip, not crash.
            printf 'PNG\x00\x01\x02\x03binary-bytes\x00here' > "$rel"
        else
            printf '%s\n' "$content" > "$rel"
        fi

        git add -A
        bash scripts/audit-layer-b.sh >/dev/null 2>&1
        echo $?
    )
}

# assert_fail: the audit MUST fail (exit != 0) for this fixture.
assert_fail() {
    local desc="$1" rel="$2" content="$3" mode="${4:-text}"
    local code
    code="$(run_audit_with_fixture "$rel" "$content" "$mode")"
    if [ "$code" != "0" ]; then
        echo "  ✅ PASS: $desc (audit failed as required, exit=$code)"
        PASS=$((PASS + 1))
    else
        echo "  ❌ FAIL: $desc (audit PASSED but should have FAILED)"
        FAIL=$((FAIL + 1))
    fi
}

# assert_pass: the audit MUST pass (exit 0) for this fixture.
assert_pass() {
    local desc="$1" rel="$2" content="$3" mode="${4:-text}"
    local code
    code="$(run_audit_with_fixture "$rel" "$content" "$mode")"
    if [ "$code" = "0" ]; then
        echo "  ✅ PASS: $desc (audit passed as required)"
        PASS=$((PASS + 1))
    else
        echo "  ❌ FAIL: $desc (audit FAILED but should have PASSED, exit=$code)"
        FAIL=$((FAIL + 1))
    fi
}

echo "═══════════════════════════════════════════════════════════"
echo "  Layer B Audit — Regression Tests"
echo "═══════════════════════════════════════════════════════════"
echo ""

NAME="$(forbidden_name)"
if [ -z "$NAME" ]; then
    echo "FATAL: could not derive a forbidden name from $AUDIT_SCRIPT" >&2
    exit 2
fi
# Embed the runtime-derived forbidden name in a harmless sentence so the fixture
# is realistic text, not just a bare token.
LEAK="internal note about ${NAME} should never ship"

# 1. tracked .txt file with a forbidden private name must FAIL
assert_fail "tracked .txt with forbidden name FAILS" "notes/secret.txt" "$LEAK"

# 2. tracked .html file with a forbidden private name must FAIL
assert_fail "tracked .html with forbidden name FAILS" "site/index.html" "<p>$LEAK</p>"

# 3. tracked .csv file with a forbidden private name must FAIL
assert_fail "tracked .csv with forbidden name FAILS" "data/people.csv" "id,name
1,$NAME"

# 4. tracked extensionless text file with a forbidden private name must FAIL
assert_fail "tracked extensionless text with forbidden name FAILS" "NOTICE" "$LEAK"

# Bonus formats the old allowlist also missed:
assert_fail "tracked .xml with forbidden name FAILS" "conf/app.xml" "<owner>$NAME</owner>"
assert_fail "tracked .sql with forbidden name FAILS" "db/seed.sql" "-- owner: $NAME"
assert_fail "tracked .ini with forbidden name FAILS" "conf/app.ini" "owner=$NAME"
assert_fail "tracked .cfg with forbidden name FAILS" "conf/app.cfg" "owner = $NAME"

# Case-insensitive: lowercased leak must still FAIL.
assert_fail "lowercased forbidden name FAILS (case-insensitive)" \
    "docs/table.txt" "owner: $(printf '%s' "$NAME" | tr '[:upper:]' '[:lower:]')"

# 5. a harmless binary-like file must NOT crash the script (audit should pass:
#    binary content is skipped, no private name in any text file).
assert_pass "harmless binary file does not crash audit" "assets/logo.png" "" "binary"

# 6. .env.test / .env.local must FAIL (real .env files, not the .example exempt).
#    These trip check #4 (.env files) regardless of content.
assert_fail ".env.test FAILS" ".env.test" "TOKEN=abc"
assert_fail ".env.local FAILS" ".env.local" "TOKEN=abc"

# 7. .example files may be exempt only if they contain no real private data:
#    an .example with NO forbidden name must PASS (exempt + clean).
assert_pass "clean .example file is exempt and passes" "familyclaw.toml.example" "owner = agent_alpha"

# Control: a clean text file with no forbidden name must PASS (no false positive).
assert_pass "clean .txt with no forbidden name passes" "notes/clean.txt" "hello world, agent_alpha here"

echo ""
echo "═══════════════════════════════════════════════════════════"
echo "  Results: $PASS passed, $FAIL failed"
echo "═══════════════════════════════════════════════════════════"
if [ "$FAIL" -eq 0 ]; then
    exit 0
else
    exit 1
fi
