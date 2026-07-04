#!/usr/bin/env bash
# FamilyClaw - Booth export (Linux/macOS). Prepare a PUBLIC-SAFE demo tree.
#
#   bash scripts/booth-export.sh [OUT_DIR]
#
# Produces a clean demo folder that is SAFE to put on a public booth machine:
#   1. builds the release demo binaries,
#   2. exports the tracked working tree via `git archive` (NO .git directory, so
#      the private git history — which leaks Layer B names — is NOT carried),
#   3. copies the prebuilt binaries into the export so the demo runs even with a
#      broken toolchain or no network,
#   4. records the source commit in <OUT_DIR>/COMMIT.txt.
#
# The export contains NO git history. Never copy the .git directory to a booth.

set -euo pipefail
cd "$(dirname "$0")/.."

OUT_DIR="${1:-booth}"
SHA="$(git rev-parse --short HEAD)"
FULL="$(git rev-parse HEAD)"
EXT=""
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) EXT=".exe" ;; esac

echo "=== FamilyClaw booth export (commit $SHA) ==="

# 1. Build release binaries.
echo "Building release demo binaries..."
cargo build --release -p familyclaw-agent --example two_agents_memory
cargo build --release -p familyclaw-agent --bin crash_replay
cargo build --release -p familyclaw-bench --bin bench

# 2. Clean the target dir and export the tracked tree WITHOUT .git.
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"
echo "Exporting tracked tree via git archive (no .git)..."
git archive --format=tar HEAD | tar -x -C "$OUT_DIR"
[ -f "$OUT_DIR/Cargo.toml" ] || { echo "git archive export failed"; exit 1; }

# 3. Copy prebuilt binaries into the export.
mkdir -p "$OUT_DIR/bin"
cp "target/release/crash_replay${EXT}" "$OUT_DIR/bin/"
cp "target/release/bench${EXT}" "$OUT_DIR/bin/"
cp "target/release/examples/two_agents_memory${EXT}" "$OUT_DIR/bin/"

# 4. Record the source commit + usage.
cat > "$OUT_DIR/COMMIT.txt" <<EOF
FamilyClaw booth export
source commit: $FULL

Prebuilt binaries in bin/ (no toolchain or network needed):
  bin/two_agents_memory${EXT}          # flagship continuity demo
  bin/crash_replay${EXT} full          # durable crash-replay proof
  bin/bench${EXT} all                  # 8-scenario deterministic scorecard

This folder has NO .git directory. Do not run 'git log' here (there is no
history to leak). Safe for a public booth machine.
EOF

# 5. Privacy assertion: there must be no .git in the export.
if [ -d "$OUT_DIR/.git" ]; then
    echo "SAFETY FAIL: .git present in export — do NOT use this on a booth."
    exit 1
fi

echo
echo "Booth export ready at: $OUT_DIR"
echo "  No .git (history not carried). Prebuilt binaries in $OUT_DIR/bin."
echo "  Fallback demo (no toolchain needed):"
echo "    $OUT_DIR/bin/crash_replay${EXT} full"
