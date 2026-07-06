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

# Forbidden real Layer B names (must never appear in publishable content)
FORBIDDEN_NAMES="agent_alpha agent_beta agent_delta agent_gamma agent_epsilon assistant \"agent_gamma Jr\" \"agent_gamma\" \"agent_gamma-jr\""

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
echo "3️⃣  Checking for hardcoded secrets..."
if grep -rE "(api_key|API_KEY|secret|token)\s*=\s*[\"']{1}[^\"']{10,}" --include="*.rs" --include="*.toml" --include="*.json" crates/ 2>/dev/null | grep -q .; then
    echo "   ❌ FAIL: Hardcoded secrets found in source"
    grep -rE "(api_key|API_KEY|secret|token)\s*=\s*[\"']{1}[^\"']{10,}" --include="*.rs" --include="*.toml" --include="*.json" crates/ 2>/dev/null
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
echo "8️⃣  Checking for real Layer B names in publishable content..."
# Scan EVERY git-tracked TEXT file instead of an extension allowlist. An
# extension allowlist ('*.md' '*.rs' …) silently missed tracked text files in
# other formats — .txt/.html/.csv/.xml/.sql/.ini/.cfg as well as extensionless
# text files (LICENSE, .gitattributes) — any of which could carry a leaked
# private name into the OSS tree. (Earlier this allowlist had already missed
# FAMILYCLAW_MAP.md + docs/plans/ + docs/research/ + docs/source-blueprints/.)
# Internal-only files must be untracked (.gitignore) — not whitelisted here.
# `docs/archive/` holds superseded historical plans (Layer B names may appear);
# quarantined from publishable scan — see MASTERPLAN.md.
#
# Text-vs-binary is decided by CONTENT, not extension: `grep -Iq .` matches a
# file iff it is text with at least one byte of content (GNU grep -I treats files
# containing NUL bytes as binary → no match). We never GUESS by extension, so a
# new binary format added tomorrow is skipped safely and a new text format is
# scanned by default. Empty files match nothing and carry no content, so skipping
# them is safe.
#
# An explicit deny-list excludes legitimately-public files that mention agent
# names only as escaped/example tokens (e.g. *.example).
# `|| true`: the trailing `grep -vE` exits 1 when EVERY tracked path is filtered
# out (e.g. a repo whose only tracked files are .example + this script). Under
# `set -e` an empty result would otherwise abort the whole audit — guard it so an
# empty scan set is treated as "nothing to scan", not as a script error.
ALL_TRACKED=$(git ls-files 2>/dev/null \
    | grep -vE '(^|/)(target)/' \
    | grep -vE '(^|/)docs/archive/' \
    | grep -vE '\.example($|\.)' \
    | grep -vE '(^|/)scripts/audit-layer-b\.sh$' || true)
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
NAME_FOUND=0
for name in $FORBIDDEN_NAMES; do
    # Remove quotes for grep
    clean_name=$(echo "$name" | sed 's/"//g')
    [ -z "$SCAN_FILES" ] && continue
    # Case-INSENSITIVE: real names leaked lowercase (agent_epsilon/agent_beta/agent_gamma in a
    # docs table) slipped past a case-sensitive scan. -i closes that gap.
    if echo "$SCAN_FILES" | xargs grep -il "$clean_name" 2>/dev/null \
        | xargs -r grep -Hi "$clean_name" 2>/dev/null \
        | grep -v "\.example" \
        | grep -vi "agent_alpha\|agent_beta\|agent_gamma\|agent_delta\|agent_epsilon\|maintainer\|operator\|user" \
        | grep -q .; then
        echo "   ❌ FAIL: Real agent name '$clean_name' found in publishable content"
        echo "$SCAN_FILES" | xargs grep -il "$clean_name" 2>/dev/null \
            | xargs -r grep -Hni "$clean_name" 2>/dev/null \
            | grep -v "\.example" \
            | grep -vi "agent_alpha\|agent_beta\|agent_gamma\|agent_delta\|agent_epsilon\|maintainer\|operator\|user"
        NAME_FOUND=1
        FAIL=1
    fi
done
if [ $NAME_FOUND -eq 0 ]; then
    echo "   ✅ PASS: No real Layer B names in publishable content"
fi

# 9. Check example agent names specifically (must be agent_a, agent_b, example_family)
echo "9️⃣  Checking example agent names..."
EXAMPLE_REAL_NAMES="agent_alpha agent_beta agent_delta agent_gamma agent_epsilon the operator"
EXAMPLE_NAME_FOUND=0
for name in $EXAMPLE_REAL_NAMES; do
    if grep -r "$name" --include="*.rs" examples/ 2>/dev/null | grep -q .; then
        echo "   ❌ FAIL: Real agent name '$name' found in examples/"
        grep -r "$name" --include="*.rs" examples/
        EXAMPLE_NAME_FOUND=1
        FAIL=1
    fi
done
# Use this check's OWN counter — NOT NAME_FOUND (which belongs to check #8) — so a
# real example leak cannot be masked by a clean #8 result.
if [ $EXAMPLE_NAME_FOUND -eq 0 ]; then
    echo "   ✅ PASS: No real agent names in examples"
fi

echo ""
echo "═══════════════════════════════════════════════════════════"
if [ $FAIL -eq 0 ]; then
    echo "  ✅ LAYER B AUDIT PASSED"
    echo "  No private souls, keys, or profiles leaked to Layer A."
    echo "═══════════════════════════════════════════════════════════"
    exit 0
else
    echo "  ❌ LAYER B AUDIT FAILED"
    echo "  Fix the above before pushing to GitHub."
    echo "═══════════════════════════════════════════════════════════"
    exit 1
fi
