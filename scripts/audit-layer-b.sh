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
# Scan EVERY git-tracked publishable file (text formats) instead of a hardcoded
# allowlist. This way any newly-added tracked file is caught by default — a narrow
# allowlist silently missed FAMILYCLAW_MAP.md + docs/plans/ + docs/research/ +
# docs/source-blueprints/, which leaked real family names into the OSS tree.
# Internal-only files must be untracked (.gitignore) — not whitelisted here.
# An explicit deny-list excludes legitimately-public files that mention agent names
# only as escaped/example tokens (e.g. *.example).
# Extension-based formats PLUS extensionless tracked text files that a future
# OSS release must also be clean of (.gitignore/.dockerignore/Dockerfile can carry
# private-name comments). Extension-only scanning silently missed these.
SCAN_FILES=$(git ls-files -- \
    '*.md' '*.rs' '*.toml' '*.yml' '*.yaml' '*.json' '*.py' '*.sh' '*.ps1' \
    '.gitignore' '**/.gitignore' '.dockerignore' '**/.dockerignore' \
    'Dockerfile' '**/Dockerfile' 'Containerfile' '**/Containerfile' 2>/dev/null \
    | grep -vE '(^|/)(target)/' \
    | grep -vE '\.example($|\.)' \
    | grep -vE '\.example\.(md|rs|toml|yml|yaml|json|py|sh|ps1)$' \
    | grep -vE '(^|/)scripts/audit-layer-b\.sh$')
NAME_FOUND=0
for name in $FORBIDDEN_NAMES; do
    # Remove quotes for grep
    clean_name=$(echo "$name" | sed 's/"//g')
    [ -z "$SCAN_FILES" ] && continue
    if echo "$SCAN_FILES" | xargs grep -l "$clean_name" 2>/dev/null \
        | xargs -r grep -H "$clean_name" 2>/dev/null \
        | grep -v "\.example" \
        | grep -v "agent_alpha\|agent_beta\|agent_gamma\|agent_delta\|agent_epsilon\|maintainer\|operator\|user" \
        | grep -q .; then
        echo "   ❌ FAIL: Real agent name '$clean_name' found in publishable content"
        echo "$SCAN_FILES" | xargs grep -l "$clean_name" 2>/dev/null \
            | xargs -r grep -Hn "$clean_name" 2>/dev/null \
            | grep -v "\.example" \
            | grep -v "agent_alpha\|agent_beta\|agent_gamma\|agent_delta\|agent_epsilon\|maintainer\|operator\|user"
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
