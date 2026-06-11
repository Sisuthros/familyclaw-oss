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
# Scan: README.md, docs/ (excluding plans/ and source-blueprints/), crates/**/*.rs, examples/, .github/
# Exclude internal design docs that contain private family history
SCAN_PATHS="README.md docs/ARCHITECTURE.md docs/LAYER_BOUNDARY.md docs/QUICKSTART.md docs/DEMO.md docs/CONTRIBUTING.md crates examples .github"
NAME_FOUND=0
for name in $FORBIDDEN_NAMES; do
    # Remove quotes for grep
    clean_name=$(echo "$name" | sed 's/"//g')
    if grep -r "$clean_name" $SCAN_PATHS --include="*.md" --include="*.rs" --include="*.toml" --include="*.yml" --include="*.yaml" --include="*.json" 2>/dev/null | grep -v "\.example" | grep -v "agent_alpha\|agent_beta\|agent_gamma\|agent_delta\|agent_epsilon\|maintainer\|operator\|user" | grep -q .; then
        echo "   ❌ FAIL: Real agent name '$clean_name' found in publishable content"
        grep -r "$clean_name" $SCAN_PATHS --include="*.md" --include="*.rs" --include="*.toml" --include="*.yml" --include="*.yaml" --include="*.json" 2>/dev/null | grep -v "\.example" | grep -v "agent_alpha\|agent_beta\|agent_gamma\|agent_delta\|agent_epsilon\|maintainer\|operator\|user"
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
for name in $EXAMPLE_REAL_NAMES; do
    if grep -r "$name" --include="*.rs" examples/ 2>/dev/null | grep -q .; then
        echo "   ❌ FAIL: Real agent name '$name' found in examples/"
        grep -r "$name" --include="*.rs" examples/
        FAIL=1
    fi
done
if [ $FAIL -eq 0 ] || [ $NAME_FOUND -eq 0 ]; then
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
