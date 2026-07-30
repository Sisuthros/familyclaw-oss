#!/usr/bin/env bash
# Pre-Publish Leak Gate
# ============================================================
# The FINAL gate before FamilyClaw is made public. It closes the gap that
# scripts/audit-layer-b.sh CANNOT close on its own:
#
#   audit-layer-b.sh scans ONLY the current working tree (`git ls-files`).
#   It NEVER inspects git HISTORY or COMMIT MESSAGES. A repo whose working
#   tree is 100% clean can still leak private family names the instant it is
#   flipped public, because every past commit (diffs + messages) ships with
#   the repo. This script is the portal that would have caught that.
#
# What this gate verifies, in order:
#   0. Working tree is clean         → delegates to audit-layer-b.sh (source of truth)
#   1. Commit MESSAGES (--all)       → no private name in any subject/body
#   2. Commit HISTORY CONTENT (-S)   → no private name introduced in any diff
#   3. High-entropy secret patterns  → no key ever committed to history
#
# The tracked working tree is authoritatively covered by check #0 (delegated
# to audit-layer-b.sh); this gate deliberately does NOT re-scan it with a
# broader list, to avoid false positives on the legitimate MIT copyright line
# and on Finnish words that share a substring with the operator name.
#
# Exit 0 = safe to publish this history as-is.
# Exit 1 = a leak exists in history/messages — publish from a CLEAN-HISTORY
#          orphan repo instead (docs/PUBLISH_ORPHAN_PLAN.md), then re-run here.
#
# ── Why this file contains no plaintext family name ────────────────────────
# This script is a tracked file scanned by audit-layer-b.sh check #8. Embedding
# a real private name here would fail that audit. The forbidden-name list is
# therefore READ AT RUNTIME from the same gitignored, operator-local file
# audit-layer-b.sh uses (scripts/audit-layer-b.names.local -- Decision Point 2,
# Option B, docs/PUBLISH_ORPHAN_PLAN.md), falling back to a tracked
# placeholder list when that file is absent, plus history-only tokens
# reassembled at runtime (never present as whole words).

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
AUDIT_SCRIPT="$SCRIPT_DIR/audit-layer-b.sh"

cd "$REPO_ROOT" || { echo "FATAL: cannot cd to repo root" >&2; exit 2; }

echo "═══════════════════════════════════════════════════════════"
echo "  Pre-Publish Leak Gate  (history + messages + tree)"
echo "═══════════════════════════════════════════════════════════"
echo ""

FAIL=0

# ── Build the forbidden-name list without ever hardcoding one ──────────────
# Source of truth: the SAME gitignored, operator-local file audit-layer-b.sh
# reads (docs/PUBLISH_ORPHAN_PLAN.md, Decision Point 2, Option B) -- not
# audit-layer-b.sh's own source text, which (as of that fix) no longer
# contains any real name to parse out. Falls back to the same tracked
# placeholder list when the local file is absent, so this gate still runs
# (with reduced real-world coverage) for contributors / CI on the public
# repo. We keep only SINGLE-word tokens (drop the quoted multi-word variant
# entries, whose embedded quotes and spaces are not valid grep-word atoms and
# are already covered by the corresponding single-word match anyway).
#
# ── 2026-07-30: no placeholder fallback in the PUBLISH gate, at all ────────
# audit-layer-b.sh keeps an explicit, loudly-announced placeholder mode so the
# checks stay exercised in public CI. This script does not, and must not: it
# exists to answer exactly one question — "is it safe to make this history
# public?" — and that question cannot be answered by scanning for
# `PlaceholderAgentOne`. A run without the real list is not a weaker answer,
# it is no answer. Missing list ⇒ exit 2, fail-closed, no opt-out.
NAMES_LOCAL_FILE="$SCRIPT_DIR/audit-layer-b.names.local"
if [ ! -f "$NAMES_LOCAL_FILE" ]; then
    echo "   ❌ FATAL: forbidden-name list not found: $NAMES_LOCAL_FILE" >&2
    echo "      Create it from scripts/audit-layer-b.names.local.example." >&2
    echo "      This gate has NO placeholder mode: a publish decision made" >&2
    echo "      against placeholder names is a false clearance, not a" >&2
    echo "      reduced-coverage one." >&2
    exit 2
fi
RAW_NAMES="$(cat "$NAMES_LOCAL_FILE")"

# History-only tokens absent from audit-layer-b.sh's working-tree list but
# present in old FI commit messages / recon-doc diffs. Built at runtime from
# fragments that individually contain NO forbidden name as a substring, so this
# very file passes audit-layer-b.sh check #8 (which greps for the whole names).
#
#  - Operator name: the FI four-letter "Vi" + "lle" (also not in the audit's
#    FORBIDDEN_NAMES, so it is a message/history-only concern).
#  - Old codename = the first audit name (derived below at runtime) + "Claw".
EXTRA_OP="$(printf 'Vi'; printf 'lle')"
FIRST_NAME=$(printf '%s\n' $RAW_NAMES | tr -d '"' | grep -E '^[A-Za-z]+$' | head -n1)
EXTRA_CODENAME="${FIRST_NAME}Claw"   # e.g. "<first-audit-name>Claw"; no literal in source

# Normalise: strip quotes, keep single alphabetic words only, dedupe.
SCAN_NAMES=$(printf '%s %s %s\n' "$RAW_NAMES" "$EXTRA_OP" "$EXTRA_CODENAME" \
    | tr -d '"' \
    | tr ' ' '\n' \
    | grep -E '^[A-Za-z]+$' \
    | sort -u)

if [ -z "$SCAN_NAMES" ]; then
    echo "   ⚠️  Could not derive names from audit-layer-b.sh — aborting (fail-closed)." >&2
    exit 2
fi

# Legitimate public tokens that must not count as a leak.
DENY='\.example|agent_alpha|agent_beta|agent_gamma|agent_delta|agent_epsilon|maintainer|operator'

# ── 0. Working-tree audit (delegate — authoritative for tracked files) ─────
echo "0️⃣  Working-tree audit (scripts/audit-layer-b.sh)…"
if [ -f "$AUDIT_SCRIPT" ]; then
    if bash "$AUDIT_SCRIPT" >"/tmp/prepub_audit.$$" 2>&1; then
        echo "   ✅ PASS: working tree clean"
    else
        echo "   ❌ FAIL: working-tree audit failed:"
        sed 's/^/      /' "/tmp/prepub_audit.$$"
        FAIL=1
    fi
    rm -f "/tmp/prepub_audit.$$"
else
    echo "   ❌ FAIL: audit-layer-b.sh not found"
    FAIL=1
fi

# ── 1. Commit MESSAGES across all refs ─────────────────────────────────────
echo "1️⃣  Scanning commit messages (--all)…"
MSG_HITS=0
ALL_MSGS=$(git log --all --format='%H%x09%s%x09%b' 2>/dev/null || true)
while IFS= read -r name; do
    [ -z "$name" ] && continue
    if printf '%s\n' "$ALL_MSGS" | grep -iE "$name" | grep -viE "$DENY" | grep -q .; then
        n=$(git log --all --oneline -i --grep="$name" 2>/dev/null | wc -l | tr -d ' ')
        echo "   ❌ FAIL: '$name' in commit messages — approx $n commit(s)"
        MSG_HITS=1
        FAIL=1
    fi
done <<< "$SCAN_NAMES"
[ $MSG_HITS -eq 0 ] && echo "   ✅ PASS: no private names in any commit message"

# ── 2. Commit HISTORY CONTENT (pickaxe over all diffs) ─────────────────────
echo "2️⃣  Scanning committed diff content (git log -S, --all)…"
# Excludes THIS script and audit-layer-b.sh's own diffs from the pickaxe
# search: both legitimately carry the forbidden-name list itself (as real
# names when the operator-local file exists, or as the tracked placeholder
# fallback when it doesn't) -- either way that line's own history will
# always self-match a search for its own words, structurally, no matter what
# words are chosen. That is the denylist declaring itself, not a leak; a
# real leak into any OTHER tracked file is still fully caught.
CONTENT_HITS=0
while IFS= read -r name; do
    [ -z "$name" ] && continue
    PICKAXE_EXCLUDES=(':(exclude)scripts/audit-layer-b.sh' ':(exclude)scripts/pre-publish-scan.sh')
    # The maintainer's own name is legitimate, intentional copyright/maintainer
    # attribution (LICENSE, GOVERNANCE.md, README.md) -- audit-layer-b.sh check
    # #8 already exempts exactly this (path exception for LICENSE/GOVERNANCE.md
    # plus a ©/copyright line filter that also covers README.md). git log -S
    # cannot filter by line content, only by whole file, so mirror that
    # exemption here -- scoped to ONLY the maintainer-name token, so every
    # other forbidden name (real private agent personas) is still fully
    # scanned in these files, same as everywhere else.
    if [ "$name" = "$EXTRA_OP" ]; then
        PICKAXE_EXCLUDES+=(':(exclude)LICENSE' ':(exclude)GOVERNANCE.md' ':(exclude)README.md')
    fi
    n=$(git log --all --oneline -S"$name" --pickaxe-regex -i -- . "${PICKAXE_EXCLUDES[@]}" \
        2>/dev/null | wc -l | tr -d ' ')
    if [ "$n" -gt 0 ]; then
        echo "   ❌ FAIL: '$name' present in $n commit(s) of history content"
        CONTENT_HITS=1
        FAIL=1
    fi
done <<< "$SCAN_NAMES"
[ $CONTENT_HITS -eq 0 ] && echo "   ✅ PASS: no private names in any committed diff"

# ── 2b. Commit AUTHOR / COMMITTER metadata across all refs ─────────────────
# The gap that made this check necessary: private agent identities were git
# commit AUTHORS. That metadata is in neither the diff content (#2) nor the
# commit messages (#1), so both existing checks reported clean — while every
# commit page and the contributors graph would have published the names the
# moment the repo went public. Author/committer identity survives every
# working-tree cleanup; only a history rewrite removes it.
echo "2️⃣b Scanning commit author/committer metadata (--all)…"
IDENT_HITS=0
ALL_IDENTS=$(git log --all --format='%H%x09%an%x09%ae%x09%cn%x09%ce' 2>/dev/null || true)
if [ -z "$ALL_IDENTS" ]; then
    echo "   ⚠️  SKIP: no history reachable"
else
    while IFS= read -r name; do
        [ -z "$name" ] && continue
        n=$(printf '%s\n' "$ALL_IDENTS" | grep -icw "$name" || true)
        if [ "$n" -gt 0 ]; then
            echo "   ❌ FAIL: '$name' in author/committer metadata of $n commit(s)"
            IDENT_HITS=1
            FAIL=1
        fi
    done <<< "$SCAN_NAMES"
    IDENT_MAILS=$(printf '%s\n' "$ALL_IDENTS" \
        | grep -oEi '[A-Za-z0-9._%+-]+@(gmail|hotmail|outlook|proton|icloud)\.[A-Za-z.]+' \
        | grep -viE 'noreply@|users\.noreply|example\.com' \
        | sort -u || true)
    if [ -n "$IDENT_MAILS" ]; then
        c=$(printf '%s\n' "$IDENT_MAILS" | grep -c . || true)
        echo "   ❌ FAIL: personal email in author/committer metadata ($c distinct)"
        printf '%s\n' "$IDENT_MAILS" | sed -E 's/^(.{2}).*@(.{2}).*$/      \1…@\2…[REDACTED]/'
        IDENT_HITS=1
        FAIL=1
    fi
    [ $IDENT_HITS -eq 0 ] && echo "   ✅ PASS: no private identity in author/committer metadata"
fi

# ── 3. High-entropy secrets ever committed to history ──────────────────────
echo "3️⃣  Scanning full history for secret patterns…"
SECRET_RE='sk-[a-zA-Z0-9]{20}|AKIA[0-9A-Z]{16}|ghp_[a-zA-Z0-9]{30,}|gho_[a-zA-Z0-9]{30,}|xox[baprs]-[a-zA-Z0-9-]{10,}|BEGIN [A-Z ]*PRIVATE KEY'
if git log --all -p 2>/dev/null | grep -aE "$SECRET_RE" | grep -q .; then
    echo "   ❌ FAIL: secret-like pattern found in git history"
    git log --all -p 2>/dev/null | grep -aoE "$SECRET_RE" | sort -u \
        | sed 's/\(.\{10\}\).*/\1…[REDACTED]/' | head
    FAIL=1
else
    echo "   ✅ PASS: no secret patterns in history"
fi

echo ""
echo "═══════════════════════════════════════════════════════════"
if [ $FAIL -eq 0 ]; then
    echo "  ✅ PRE-PUBLISH GATE PASSED — safe to publish this history."
    echo "═══════════════════════════════════════════════════════════"
    exit 0
else
    echo "  ❌ PRE-PUBLISH GATE FAILED — DO NOT make public."
    echo "  Publish from a CLEAN-HISTORY orphan repo instead (see"
    echo "  docs/PUBLISH_ORPHAN_PLAN.md), then re-run this gate there."
    echo "═══════════════════════════════════════════════════════════"
    exit 1
fi
