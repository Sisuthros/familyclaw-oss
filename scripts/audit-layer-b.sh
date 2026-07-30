#!/usr/bin/env bash
# Layer B Leak Audit
# ============================================================
# Ensures no Layer B (private) content reaches Layer A (OSS) repo
# Run in CI and as pre-push hook

set -e

echo "═══════════════════════════════════════════════════════════"
echo "  Layer B Leak Audit"
echo "═══════════════════════════════════════════════════════════"
echo ""

FAIL=0

# Forbidden real Layer B names (must never appear in publishable content).
#
# This file is itself published (Layer A), so the real private names are
# NEVER hardcoded here (docs/PUBLISH_ORPHAN_PLAN.md, Decision Point 2,
# Option B) — doing so would fail this very audit's own check #8, and would
# make any single-commit orphan-publish snapshot structurally unable to pass
# scripts/pre-publish-scan.sh's diff-content scan (the denylist string itself
# would always be the leak). The real list lives in a gitignored,
# operator-local file that this script sources when present; without it, a
# placeholder list keeps every check below exercised (still useful for
# contributors / CI on the public repo), just without real-world coverage.
# See scripts/audit-layer-b.names.local.example for the format.
#
# Two of the real entries happen to be common English/astronomy words, so the
# existing -w word-boundary match plus the embedded-word post-filter below
# (which already drops e.g. "innovation") is what keeps this from drowning in
# false positives — no additional filtering needed beyond what checks
# already do for every other forbidden name.
#
# ── 2026-07-30: the silent fallback is GONE ────────────────────────────────
# This block used to fall back to the placeholder list WITHOUT SAYING SO when
# the operator-local file was absent. That produced a real false PASS: the
# audit reported "✅ No real Layer B names" while scanning for strings like
# `PlaceholderAgentOne` that of course appear nowhere. The gate was not green,
# it was blind — and it was cited as publish clearance.
#
# There is now no implicit path. Exactly one of these must hold:
#   a) scripts/audit-layer-b.names.local exists  → REAL mode (full coverage);
#   b) FAMILYCLAW_AUDIT_ALLOW_PLACEHOLDER_NAMES=1 is set explicitly
#      → PLACEHOLDER mode, announced loudly in the output and in the final
#        banner. Intended ONLY for CI on the public repo, where the real list
#        cannot exist. A placeholder run must never be cited as clearance.
#   c) neither → HARD FAIL, exit 2. Silence is no longer an option.
FORBIDDEN_NAMES_PLACEHOLDER="PlaceholderAgentOne PlaceholderAgentTwo PlaceholderAgentThree PlaceholderPersonFour PlaceholderPersonFive"
FORBIDDEN_NAMES_LOCAL_FILE="scripts/audit-layer-b.names.local"
if [ -f "$FORBIDDEN_NAMES_LOCAL_FILE" ]; then
    FORBIDDEN_NAMES="$(cat "$FORBIDDEN_NAMES_LOCAL_FILE")"
    NAMES_MODE="real"
    echo "   ℹ️  Name list: $FORBIDDEN_NAMES_LOCAL_FILE (REAL mode, full coverage)"
elif [ "${FAMILYCLAW_AUDIT_ALLOW_PLACEHOLDER_NAMES:-0}" = "1" ]; then
    FORBIDDEN_NAMES="$FORBIDDEN_NAMES_PLACEHOLDER"
    NAMES_MODE="placeholder"
    echo "   ⚠️  PLACEHOLDER MODE — reduced coverage."
    echo "      $FORBIDDEN_NAMES_LOCAL_FILE is absent and"
    echo "      FAMILYCLAW_AUDIT_ALLOW_PLACEHOLDER_NAMES=1 was set explicitly."
    echo "      This run exercises the checks but CANNOT prove the absence of"
    echo "      real private names. DO NOT cite it as publish clearance."
else
    echo ""
    echo "   ❌ FATAL: forbidden-name list not found and no explicit opt-out."
    echo "      Expected: $FORBIDDEN_NAMES_LOCAL_FILE"
    echo "      Create it from scripts/audit-layer-b.names.local.example (it is"
    echo "      gitignored), or — only for CI on the public repo — set"
    echo "      FAMILYCLAW_AUDIT_ALLOW_PLACEHOLDER_NAMES=1 to accept reduced"
    echo "      coverage knowingly."
    echo ""
    echo "      Refusing to run: an audit with no real name list reports PASS"
    echo "      while scanning for nothing. That false PASS is worse than no"
    echo "      audit at all, because it gets quoted as evidence."
    echo "═══════════════════════════════════════════════════════════"
    exit 2
fi
echo ""

# Explicit allowlist for the email-address check (§9). Empty by default —
# populate only with specific, deliberately-public addresses that would
# otherwise false-positive (e.g. a documented public contact address).
# Format: bare email strings, one concern per entry, space-separated.
EMAIL_ALLOWLIST=""

check_dir() {
    local dir_name="$1"
    local label="$2"
    if find . -name ".git" -prune -o -name "target" -prune -o -type d -name "$dir_name" -print 2>/dev/null | grep -q .; then
        echo "   ❌ FAIL: $label directory found"
        find . -name ".git" -prune -o -name "target" -prune -o -type d -name "$dir_name" -print 2>/dev/null
        FAIL=1
    else
        echo "   ✅ PASS: No $label/ directory in repo"
    fi
}

# 1. No soul files
echo "1️⃣  Checking for soul files..."
if find . -name ".git" -prune -o -name "target" -prune -o -name "*.soul" -print -o -name "*.soul.md" -print -o -name "SOUL.md" -print 2>/dev/null | grep -q .; then
    echo "   ❌ FAIL: Soul files found outside docs/"
    find . -name ".git" -prune -o -name "target" -prune -o -name "*.soul" -print -o -name "*.soul.md" -print -o -name "SOUL.md" -print 2>/dev/null
    FAIL=1
else
    echo "   ✅ PASS: No soul files in repo"
fi

# 2. No calibration files
echo "2️⃣  Checking for calibration files..."
if find . -name ".git" -prune -o -name "target" -prune -o -name "*.calibration.json" -print 2>/dev/null | grep -q .; then
    echo "   ❌ FAIL: Calibration files found"
    find . -name ".git" -prune -o -name "target" -prune -o -name "*.calibration.json" -print 2>/dev/null
    FAIL=1
else
    echo "   ✅ PASS: No calibration files in repo"
fi

# 3. No hardcoded secrets (actual values, not field names)
# Widened repo-wide (was crates/-only, which missed secrets leaking into
# top-level scripts, docs, or config). Scans all git-TRACKED files of the
# given extensions, repo-wide, excluding docs/archive/ (quarantined historical
# content, consistent with the §8 scan below) and test-fixture patterns that
# are already handled by more specific excludes elsewhere in this script.
echo "3️⃣  Checking for hardcoded secrets..."
SECRET_SCAN_FILES=$(git ls-files 2>/dev/null \
    | grep -vE '(^|/)(target)/' \
    | grep -vE '(^|/)docs/archive/' \
    | grep -E '\.(rs|toml|json|md|sh|ps1|py)$' || true)
SECRET_PATTERN="(api_key|API_KEY|secret|token)\s*=\s*[\"']{1}[^\"']{10,}"
# Obvious documentation placeholders are not secrets: values that literally say
# placeholder/example/demo/dummy/changeme, angle-bracket fill-ins ("<your-key>"),
# and the repo's known safe test fixtures. Anything else that matches the
# pattern is treated as a real leak.
SECRET_PLACEHOLDER_FILTER='placeholder|example|local-demo|demo-token|dummy|changeme|your[-_]|<[^\"'"'"']*>|s3cret-token|sk-livelivelive|shh-its-a-secret'
SECRET_MATCHES=$(echo "$SECRET_SCAN_FILES" | xargs -r grep -nE "$SECRET_PATTERN" 2>/dev/null | grep -viE "$SECRET_PLACEHOLDER_FILTER" || true)
if [ -n "$SECRET_MATCHES" ]; then
    echo "   ❌ FAIL: Hardcoded secrets found in source"
    echo "$SECRET_MATCHES"
    FAIL=1
else
    echo "   ✅ PASS: No hardcoded secrets in source"
fi

# 4. No .env files
echo "4️⃣  Checking for .env files..."
if find . -name ".git" -prune -o -name "target" -prune -o -name ".env" -print -o -name ".env.*" -print 2>/dev/null | grep -v "\.env\.example" | grep -q .; then
    echo "   ❌ FAIL: .env files found"
    find . -name ".git" -prune -o -name "target" -prune -o -name ".env" -print -o -name ".env.*" -print 2>/dev/null | grep -v "\.env\.example"
    FAIL=1
else
    echo "   ✅ PASS: No .env files in repo"
fi

# 5. No profiles directory
check_dir "profiles" "profiles"

# 6. No hearth directory
check_dir "hearth" "hearth"

# 7. No keys directory
check_dir "keys" "keys"

# 8. No real agent names in publishable content
echo "8️⃣ Checking for real Layer B names in publishable content..."
# Scan EVERY git-tracked TEXT file instead of an extension allowlist. An
# extension allowlist ('*.md' '*.rs' …) silently missed tracked text files in
# other formats — .txt/.html/.csv/.xml/.sql/.ini/.cfg as well as extensionless
# text files (LICENSE, .gitattributes) — any of which could carry a leaked
# private name into the OSS tree. (Earlier this allowlist had already missed
# FAMILYCLAW_MAP.md + docs/plans/ + docs/research/ + docs/source-blueprints/.)
# Internal-only files must be untracked (.gitignore) — not whitelisted here.
# `docs/archive/` holds superseded historical plans (Layer B names may appear);
# quarantined from publishable scan — see MASTERPLAN.md.
# `docs/GIT_CONSOLIDATION.md` references real git branch names (may contain
# forbidden substrings) — internal ops only, not publishable marketing.
#
# Text-vs-binary is decided by CONTENT, not extension: `grep -Iq .` matches a
# file iff it is text with at least one byte of content (GNU grep -I treats files
# containing NUL bytes as binary → no match). We never GUESS by extension, so a
# new binary format added tomorrow is skipped safely and a new text format is
# scanned by default. Empty files match nothing and carry no content, so skipping
# them is safe.
#
# An explicit deny-list excludes legitimately-public files that mention agent
# names only as escaped/example tokens (e.g. *.example). scripts/pre-publish-scan.sh
# is excluded for the same reason this file excludes itself below: it embeds
# the SAME tracked placeholder fallback list this file does (both scripts
# resolve real names from the same gitignored local file; without it, both
# fall back to identical placeholder tokens), so it would otherwise always
# self-match here -- not a leak, just the placeholder list declaring itself.
# `|| true`: the trailing `grep -vE` exits 1 when EVERY tracked path is filtered
# out (e.g. a repo whose only tracked files are .example + this script). Under
# `set -e` an empty result would otherwise abort the whole audit — guard it so an
# empty scan set is treated as "nothing to scan", not as a script error.
ALL_TRACKED=$(git ls-files 2>/dev/null \
    | grep -vE '(^|/)(target)/' \
    | grep -vE '(^|/)docs/archive/' \
    | grep -vE '(^|/)docs/GIT_CONSOLIDATION\.md$' \
    | grep -vE '\.example($|\.)' \
    | grep -vE '(^|/)scripts/audit-layer-b\.sh$' \
    | grep -vE '(^|/)scripts/pre-publish-scan\.sh$' || true)
# Keep only TEXT files (content-based, no extension guessing). Iterate safely
# even with spaces/odd chars via a while-read loop on NUL-free, newline-listed
# paths from git ls-files (git paths use forward slashes, no newlines).
SCAN_FILES=""
while IFS= read -r f; do
    [ -z "$f" ] && continue
    [ -f "$f" ] || continue
    if grep -Iq . "$f" 2>/dev/null; then
        SCAN_FILES="${SCAN_FILES}${f}"$'\n'
    fi
done <<< "$ALL_TRACKED"
SCAN_FILES=$(printf '%s' "$SCAN_FILES")
# Precise exclusion for the copyright/maintainer-attribution surface: the
# repo's LICENSE and GOVERNANCE.md legitimately carry the maintainer's real
# name once each (copyright line, maintainer table) — that is not a leak.
# Everything else must be free of it. A blanket line-filter like
# `grep -vi "maintainer|operator|user"` is too broad: it silently swallows
# any line merely containing those common English words (e.g. "the operator
# runs this command") regardless of which file it's in, which can mask a
# genuine leak elsewhere. Instead we exclude by FILE PATH (only LICENSE and
# GOVERNANCE.md), not by line content, for the maintainer's own name — and
# keep a narrow, still content-based allowance only for the known example
# agent identifiers used throughout Layer A sample content.
NAME_FOUND=0
for name in $FORBIDDEN_NAMES; do
    # Remove quotes for grep
    clean_name=$(echo "$name" | sed 's/"//g')
    [ -z "$SCAN_FILES" ] && continue
    # Case-INSENSITIVE: real names leaked lowercase in a docs table slipped
    # past a case-sensitive scan. -i closes that gap.
    # -w (word boundary): required because one real forbidden name (an FI
    # given name) is a substring of common Finnish inflected word forms
    # (case endings attach directly to the stem, with no separator) —
    # substring matching would drown the audit in false positives.
    MATCHES=$(echo "$SCAN_FILES" | xargs grep -ilw "$clean_name" 2>/dev/null \
        | xargs -r grep -Hniw "$clean_name" 2>/dev/null \
        | grep -v "\.example" \
        | grep -vi "agent_alpha\|agent_beta\|agent_gamma\|agent_delta\|agent_epsilon" || true)
    # grep -w treats non-ASCII letters (ä/ö) as word boundaries in the C locale,
    # so Finnish inflected forms of that FI given name (case endings glued
    # directly onto the stem) false-positive on it. Drop lines where the name
    # only appears embedded in a longer word (immediately preceded/followed
    # by a letter incl. ä/ö/å).
    if [ -n "$MATCHES" ]; then
        MATCHES=$(echo "$MATCHES" | grep -viE "[a-zA-ZäöåÄÖÅ]${clean_name}|${clean_name}[a-zA-ZäöåÄÖÅ]" || true)
    fi
    # Maintainer-name exception: ONLY the LICENSE and GOVERNANCE.md files may
    # legitimately carry the real maintainer name (copyright holder /
    # maintainer-of-record attribution). Filter by path, not by a generic
    # word — this cannot mask a leak in any other file.
    if [ -n "$MATCHES" ]; then
        MATCHES=$(echo "$MATCHES" | grep -vE '^(LICENSE|GOVERNANCE\.md):' || true)
        # A copyright/attribution line (©/Copyright) carrying the maintainer name
        # is legitimate attribution, not a Layer B leak, wherever it appears.
        MATCHES=$(echo "$MATCHES" | grep -viE '©|copyright' || true)
    fi
    if [ -n "$MATCHES" ]; then
        echo "   ❌ FAIL: Real agent name '$clean_name' found in publishable content"
        echo "$MATCHES"
        NAME_FOUND=1
        FAIL=1
    fi
done
if [ $NAME_FOUND -eq 0 ]; then
    echo "   ✅ PASS: No real Layer B names in publishable content"
fi

# 9. No leaked personal email addresses in publishable content
echo "9️⃣ Checking for personal email addresses..."
# Generic personal-email-provider pattern. Legitimate, non-personal addresses
# (GitHub noreply, example.com placeholders) are excluded by pattern; specific
# deliberate exceptions can be added to EMAIL_ALLOWLIST above.
EMAIL_PATTERN='[A-Za-z0-9._%+-]+@(gmail|hotmail|outlook|proton|icloud)\.[A-Za-z.]+'
EMAIL_FOUND=0
if [ -n "$SCAN_FILES" ]; then
    RAW_EMAIL_MATCHES=$(echo "$SCAN_FILES" | xargs -r grep -nEi "$EMAIL_PATTERN" 2>/dev/null || true)
    if [ -n "$RAW_EMAIL_MATCHES" ]; then
        FILTERED_EMAIL_MATCHES="$RAW_EMAIL_MATCHES"
        # Always-allowed non-personal patterns regardless of provider domain.
        FILTERED_EMAIL_MATCHES=$(echo "$FILTERED_EMAIL_MATCHES" | grep -vi "noreply@\|users\.noreply\|example\.com" || true)
        # Apply explicit allowlist entries, if any.
        for allowed in $EMAIL_ALLOWLIST; do
            [ -z "$allowed" ] && continue
            FILTERED_EMAIL_MATCHES=$(echo "$FILTERED_EMAIL_MATCHES" | grep -vFi "$allowed" || true)
        done
        if [ -n "$FILTERED_EMAIL_MATCHES" ]; then
            echo "   ❌ FAIL: Personal email address found in publishable content"
            echo "$FILTERED_EMAIL_MATCHES"
            EMAIL_FOUND=1
            FAIL=1
        fi
    fi
fi
if [ $EMAIL_FOUND -eq 0 ]; then
    echo "   ✅ PASS: No personal email addresses in publishable content"
fi

# 10. Check example agent names specifically (must be agent_a, agent_b, example_family)
echo "🔟 Checking example agent names..."
# Reuses $FORBIDDEN_NAMES (already resolved above from the gitignored local
# file, or the tracked placeholder list) instead of a second hardcoded name
# list -- one fewer place a real name could ever be typed into this
# published file.
EXAMPLE_NAME_FOUND=0
for name in $FORBIDDEN_NAMES; do
    name=$(echo "$name" | sed 's/"//g')
    [ -z "$name" ] && continue
    if grep -r "$name" --include="*.rs" examples/ 2>/dev/null | grep -q .; then
        echo "   ❌ FAIL: Real agent name '$name' found in examples/"
        grep -r "$name" --include="*.rs" examples/
        EXAMPLE_NAME_FOUND=1
        FAIL=1
    fi
done
# Use this check's OWN counter — NOT NAME_FOUND (which belongs to check #8) — so a
# real example leak cannot be masked by a clean #8 result. (Renumbered: this is
# now check #10; personal-email check is #9.)
if [ $EXAMPLE_NAME_FOUND -eq 0 ]; then
    echo "   ✅ PASS: No real agent names in examples"
fi

# 11. No private home-drive path patterns (persona home paths are Layer B
# regardless of what name is used — a leak like `E:\SomeName\home\research\...`
# is disqualifying even if the name itself were renamed to something innocuous,
# because the drive-letter + `\home\` shape itself reveals a private local
# deployment layout). Scan tracked *.rs/*.md/*.toml (source + docs + config),
# excluding docs/archive/ (quarantined historical content, same rationale as
# the §8 scan above) since this check is about what's live/publishable today.
echo "1️⃣1️⃣ Checking for private home-drive path patterns..."
HOME_PATH_PATTERN='[A-Z]:\\[A-Za-z]+\\home\\'
HOME_PATH_FILES=$(git ls-files 2>/dev/null \
    | grep -vE '(^|/)(target)/' \
    | grep -vE '(^|/)docs/archive/' \
    | grep -E '\.(rs|md|toml)$' || true)
HOME_PATH_MATCHES=""
if [ -n "$HOME_PATH_FILES" ]; then
    HOME_PATH_MATCHES=$(echo "$HOME_PATH_FILES" | xargs -r grep -nE "$HOME_PATH_PATTERN" 2>/dev/null || true)
fi
if [ -n "$HOME_PATH_MATCHES" ]; then
    echo "   ❌ FAIL: Private home-drive path pattern found in publishable content"
    echo "$HOME_PATH_MATCHES"
    FAIL=1
else
    echo "   ✅ PASS: No private home-drive path patterns in publishable content"
fi

# 12. Commit AUTHOR / COMMITTER metadata across the full history.
#
# The blind spot that made this necessary (2026-07-30): three private agent
# identities were literal git commit AUTHORS. Neither this script nor
# pre-publish-scan.sh looked at %an/%ae/%cn/%ce at all — they scanned file
# contents and commit messages only. Author metadata is not in any diff and
# not in any message, so both gates reported clean while every commit page and
# the contributors graph would have published the names on day one.
#
# Scanned: author name, author email, committer name, committer email, over
# ALL refs. Deliberately NOT limited to the working tree: unlike every check
# above, this metadata ships with the repository itself and cannot be fixed by
# editing a file — only by rewriting history.
echo "1️⃣2️⃣ Checking commit author/committer metadata (full history)…"
IDENTITY_FOUND=0
ALL_IDENTITIES=$(git log --all --format='%H%x09%an%x09%ae%x09%cn%x09%ce' 2>/dev/null || true)
if [ -z "$ALL_IDENTITIES" ]; then
    echo "   ⚠️  SKIP: no git history reachable (not a repo, or no commits)"
else
    for name in $FORBIDDEN_NAMES; do
        clean_name=$(echo "$name" | sed 's/"//g')
        [ -z "$clean_name" ] && continue
        HITS=$(printf '%s\n' "$ALL_IDENTITIES" | grep -icw "$clean_name" || true)
        if [ "$HITS" -gt 0 ]; then
            echo "   ❌ FAIL: '$clean_name' appears in author/committer metadata of $HITS commit(s)"
            IDENTITY_FOUND=1
            FAIL=1
        fi
    done
    # Personal email providers in author/committer email, same rule as §9.
    MAIL_HITS=$(printf '%s\n' "$ALL_IDENTITIES" \
        | grep -oEi '[A-Za-z0-9._%+-]+@(gmail|hotmail|outlook|proton|icloud)\.[A-Za-z.]+' \
        | grep -viE 'noreply@|users\.noreply|example\.com' \
        | sort -u || true)
    if [ -n "$MAIL_HITS" ]; then
        COUNT=$(printf '%s\n' "$MAIL_HITS" | grep -c . || true)
        echo "   ❌ FAIL: personal email address in author/committer metadata ($COUNT distinct)"
        # Redacted: prove the finding without republishing the address.
        printf '%s\n' "$MAIL_HITS" | sed -E 's/^(.{2}).*@(.{2}).*$/      \1…@\2…[REDACTED]/'
        IDENTITY_FOUND=1
        FAIL=1
    fi
    if [ $IDENTITY_FOUND -eq 0 ]; then
        TOTAL=$(printf '%s\n' "$ALL_IDENTITIES" | grep -c . || true)
        echo "   ✅ PASS: no private identity in author/committer metadata ($TOTAL commits scanned)"
    fi
fi

echo ""
echo "═══════════════════════════════════════════════════════════"
if [ $FAIL -eq 0 ]; then
    if [ "$NAMES_MODE" = "placeholder" ]; then
        echo "  ⚠️  LAYER B AUDIT PASSED — PLACEHOLDER MODE (reduced coverage)"
        echo "  Checks were exercised, but the real name list was NOT present."
        echo "  This result is NOT publish clearance."
    else
        echo "  ✅ LAYER B AUDIT PASSED  (real name list, full coverage)"
        echo "  No private souls, keys, profiles or identities leaked to Layer A."
    fi
    echo "═══════════════════════════════════════════════════════════"
    exit 0
else
    echo "  ❌ LAYER B AUDIT FAILED"
    echo "  Fix the above before pushing to GitHub."
    echo "═══════════════════════════════════════════════════════════"
    exit 1
fi
