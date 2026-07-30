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
PREPUBLISH_SCRIPT="$REPO_ROOT/scripts/pre-publish-scan.sh"

if [ ! -f "$AUDIT_SCRIPT" ]; then
    echo "FATAL: audit script not found at $AUDIT_SCRIPT" >&2
    exit 2
fi
if [ ! -f "$PREPUBLISH_SCRIPT" ]; then
    echo "FATAL: pre-publish gate not found at $PREPUBLISH_SCRIPT" >&2
    exit 2
fi

PASS=0
FAIL=0

# Derive the forbidden fixture name from the audit's OWN list at runtime, so
# this test file never contains a real private name in plaintext.
#
# The source is deliberately FORBIDDEN_NAMES_PLACEHOLDER, not FORBIDDEN_NAMES:
# the audit resolves FORBIDDEN_NAMES at run time from the gitignored
# operator-local file `scripts/audit-layer-b.names.local` when it exists, and
# falls back to the placeholder list when it does not. `run_audit_with_fixture`
# only copies the audit SCRIPT into its sandbox — never the local names file —
# so inside every sandbox the effective list is always the placeholder list.
# Deriving from the placeholder therefore matches what the sandboxed audit
# actually scans for, and keeps this file free of real names by construction.
forbidden_name() {
    grep -E '^[[:space:]]*FORBIDDEN_NAMES_PLACEHOLDER=' "$AUDIT_SCRIPT" \
        | head -n1 \
        | sed -E 's/^[[:space:]]*FORBIDDEN_NAMES_PLACEHOLDER="?//; s/".*$//' \
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
        # The sandbox never carries the operator-local names file, so it runs
        # in the explicit placeholder mode the audit now requires. Without this
        # the audit correctly refuses to run at all (exit 2) — which is the
        # behaviour asserted separately by the meta-tests at the bottom.
        FAMILYCLAW_AUDIT_ALLOW_PLACEHOLDER_NAMES=1 \
            bash scripts/audit-layer-b.sh >/dev/null 2>&1
        echo $?
    )
}

# Build a sandbox whose COMMITS carry a caller-chosen author/committer identity,
# then run the audit. Used to prove check #12 (author metadata) actually fires —
# the blind spot that let private identities through as commit authors.
run_audit_with_commit_identity() {
    # $1 = author/committer name, $2 = author/committer email
    local ident_name="$1"
    local ident_mail="$2"
    local sandbox
    sandbox="$(mktemp -d)"

    (
        cd "$sandbox" || exit 99
        git init -q
        git config user.name "$ident_name"
        git config user.email "$ident_mail"
        mkdir -p scripts
        cp "$AUDIT_SCRIPT" scripts/audit-layer-b.sh
        printf 'clean content, nothing forbidden here\n' > NOTES.txt
        git add -A
        # Content and message are deliberately CLEAN. The only Layer B material
        # is the identity — exactly the case both gates used to miss.
        git -c commit.gpgsign=false commit -q -m "chore: add notes" >/dev/null 2>&1
        FAMILYCLAW_AUDIT_ALLOW_PLACEHOLDER_NAMES=1 \
            bash scripts/audit-layer-b.sh >/dev/null 2>&1
        echo $?
    )
}

# Run the audit in a sandbox with NO names file and NO opt-out env var.
# Must refuse to run (exit 2) rather than silently passing on placeholders.
run_audit_without_name_list() {
    local sandbox
    sandbox="$(mktemp -d)"
    (
        cd "$sandbox" || exit 99
        git init -q
        git config user.email "test@example.com"
        git config user.name "audit-test"
        mkdir -p scripts
        cp "$AUDIT_SCRIPT" scripts/audit-layer-b.sh
        printf 'hello\n' > NOTES.txt
        git add -A
        env -u FAMILYCLAW_AUDIT_ALLOW_PLACEHOLDER_NAMES \
            bash scripts/audit-layer-b.sh >/dev/null 2>&1
        echo $?
    )
}

# Run the PUBLISH gate in a sandbox with no names file. It must refuse
# unconditionally — it has no placeholder mode, even with the opt-out set.
run_prepublish_without_name_list() {
    local sandbox
    sandbox="$(mktemp -d)"
    (
        cd "$sandbox" || exit 99
        git init -q
        git config user.email "test@example.com"
        git config user.name "audit-test"
        mkdir -p scripts
        cp "$AUDIT_SCRIPT" scripts/audit-layer-b.sh
        cp "$PREPUBLISH_SCRIPT" scripts/pre-publish-scan.sh
        printf 'hello\n' > NOTES.txt
        git add -A
        git -c commit.gpgsign=false commit -q -m "chore: init" >/dev/null 2>&1
        # Opt-out DELIBERATELY set: the publish gate must ignore it.
        FAMILYCLAW_AUDIT_ALLOW_PLACEHOLDER_NAMES=1 \
            bash scripts/pre-publish-scan.sh >/dev/null 2>&1
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

# ── Check #12: commit AUTHOR / COMMITTER metadata ──────────────────────────
# These prove the gap found on 2026-07-30 is closed. In each case the working
# tree and the commit message are CLEAN — the only Layer B material is the
# commit identity. Before check #12 existed, every one of these passed.
echo ""
echo "  ── author/committer metadata (check #12) ──"

assert_identity_fail() {
    local desc="$1" ident_name="$2" ident_mail="$3"
    local code
    code="$(run_audit_with_commit_identity "$ident_name" "$ident_mail")"
    if [ "$code" != "0" ]; then
        echo "  ✅ PASS: $desc (audit failed as required, exit=$code)"
        PASS=$((PASS + 1))
    else
        echo "  ❌ FAIL: $desc (audit PASSED but should have FAILED)"
        FAIL=$((FAIL + 1))
    fi
}

assert_identity_pass() {
    local desc="$1" ident_name="$2" ident_mail="$3"
    local code
    code="$(run_audit_with_commit_identity "$ident_name" "$ident_mail")"
    if [ "$code" = "0" ]; then
        echo "  ✅ PASS: $desc (audit passed as required)"
        PASS=$((PASS + 1))
    else
        echo "  ❌ FAIL: $desc (audit FAILED but should have PASSED, exit=$code)"
        FAIL=$((FAIL + 1))
    fi
}

# Forbidden name as the commit AUTHOR NAME → must FAIL.
assert_identity_fail "forbidden name as commit author name FAILS" \
    "$NAME" "$(printf '%s' "$NAME" | tr '[:upper:]' '[:lower:]')@example.com"

# Forbidden name only inside the author EMAIL local part → must FAIL.
assert_identity_fail "forbidden name inside author email FAILS" \
    "Some Contributor" "$(printf '%s' "$NAME" | tr '[:upper:]' '[:lower:]')@familyclaw.local"

# A personal gmail address as the author email → must FAIL, even with a
# perfectly innocuous author name.
assert_identity_fail "personal gmail as author email FAILS" \
    "Some Contributor" "someone.private@gmail.com"

# Control: a neutral identity on a GitHub noreply address must PASS. This is
# exactly the replacement identity used by the history rewrite, so this case
# doubles as a regression guard against the rewrite being undone.
assert_identity_pass "neutral noreply identity passes" \
    "FamilyClaw Contributor" "noreply@users.noreply.github.com"

# ── Missing name list must be a HARD FAIL, never a silent PASS ─────────────
# The false-PASS bug in full: with no operator-local list the audit quietly
# scanned for placeholder strings, found none, and printed
# "✅ No real Layer B names in publishable content". That output was then
# cited as publish clearance for a repo that was leaking seven name
# categories. Both gates must now refuse rather than reassure.
echo ""
echo "  ── missing name list must fail closed ──"

code="$(run_audit_without_name_list)"
if [ "$code" = "2" ]; then
    echo "  ✅ PASS: audit refuses to run without a name list (exit=2)"
    PASS=$((PASS + 1))
else
    echo "  ❌ FAIL: audit ran without a name list (exit=$code, expected 2)"
    FAIL=$((FAIL + 1))
fi

code="$(run_prepublish_without_name_list)"
if [ "$code" = "2" ]; then
    echo "  ✅ PASS: publish gate refuses placeholder mode outright (exit=2)"
    PASS=$((PASS + 1))
else
    echo "  ❌ FAIL: publish gate accepted a missing name list (exit=$code, expected 2)"
    FAIL=$((FAIL + 1))
fi

echo ""
echo "═══════════════════════════════════════════════════════════"
echo "  Results: $PASS passed, $FAIL failed"
echo "═══════════════════════════════════════════════════════════"
if [ "$FAIL" -eq 0 ]; then
    exit 0
else
    exit 1
fi
