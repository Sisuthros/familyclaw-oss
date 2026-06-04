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

# 1. No soul files
echo "1️⃣  Checking for soul files..."
if find . -name "*.soul" -o -name "*.soul.md" | grep -v docs | grep -v ".example" | grep -q .; then
    echo "   ❌ FAIL: Soul files found outside docs/"
    find . -name "*.soul" -o -name "*.soul.md" | grep -v docs | grep -v ".example"
    FAIL=1
else
    echo "   ✅ PASS: No soul files in repo"
fi

# 2. No SOUL.md except examples
echo "2️⃣  Checking for SOUL.md..."
if find . -name "SOUL.md" | grep -v docs | grep -v ".example" | grep -q .; then
    echo "   ❌ FAIL: SOUL.md found outside docs/"
    find . -name "SOUL.md" | grep -v docs | grep -v ".example"
    FAIL=1
else
    echo "   ✅ PASS: No private SOUL.md in repo"
fi

# 3. No calibration files
echo "3️⃣  Checking for calibration files..."
if find . -name "*.calibration.json" | grep -v docs | grep -v ".example" | grep -q .; then
    echo "   ❌ FAIL: Calibration files found"
    find . -name "*.calibration.json" | grep -v docs | grep -v ".example"
    FAIL=1
else
    echo "   ✅ PASS: No calibration files in repo"
fi

# 4. No hardcoded secrets (actual values, not field names)
echo "4️⃣  Checking for hardcoded secrets..."
if grep -rE "(api_key|API_KEY|secret|token)\s*=\s*[\"'"'"'][^\"'"'"']{10,}" --include="*.rs" --include="*.toml" --include="*.json" crates/ | grep -q .; then
    echo "   ❌ FAIL: Hardcoded secrets found in source"
    grep -rE "(api_key|API_KEY|secret|token)\s*=\s*[\"'"'"'][^\"'"'"']{10,}" --include="*.rs" --include="*.toml" --include="*.json" crates/
    FAIL=1
else
    echo "   ✅ PASS: No hardcoded secrets in source"
fi

# 5. No .env files
echo "5️⃣  Checking for .env files..."
if find . -name ".env" -o -name ".env.*" | grep -v "\.env\.example" | grep -q .; then
    echo "   ❌ FAIL: .env files found"
    find . -name ".env" -o -name ".env.*" | grep -v "\.env\.example"
    FAIL=1
else
    echo "   ✅ PASS: No .env files in repo"
fi

# 6. No profiles directory
echo "6️⃣  Checking for profiles directory..."
if find . -type d -name "profiles" | grep -v ".git" | grep -q .; then
    echo "   ❌ FAIL: profiles/ directory found"
    find . -type d -name "profiles" | grep -v ".git"
    FAIL=1
else
    echo "   ✅ PASS: No profiles/ directory in repo"
fi

# 7. No hearth directory (family memory)
echo "7️⃣  Checking for hearth directory..."
if find . -type d -name "hearth" | grep -v ".git" | grep -q .; then
    echo "   ❌ FAIL: hearth/ directory found"
    find . -type d -name "hearth" | grep -v ".git"
    FAIL=1
else
    echo "   ✅ PASS: No hearth/ directory in repo"
fi

# 8. No real agent names in examples (must be agent_a, agent_b, example_family)
echo "8️⃣  Checking example agent names..."
REAL_NAMES="agent_alpha agent_beta agent_delta agent_gamma agent_epsilon the operator"
for name in $REAL_NAMES; do
    if grep -r "$name" --include="*.rs" examples/ 2>/dev/null | grep -q .; then
        echo "   ❌ FAIL: Real agent name '$name' found in examples/"
        grep -r "$name" --include="*.rs" examples/
        FAIL=1
    fi
done
if [ $FAIL -eq 0 ]; then
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