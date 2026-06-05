#!/usr/bin/env bash
# bench.sh — yhden komennon FamilyClaw-jatkuvuusbenchmarkki.
#
# Rakentaa continuity_daemon-binäärin (musta laatikko jota harness ajaa),
# sitten ajaa kaikki skenaariot (S1 Crash Matrix, S2 Retention Curve,
# S3 Dream Quality) kiinteällä injektoidulla kellolla ja kirjoittaa:
#   - crates/familyclaw-bench/out/scorecard.json
#   - crates/familyclaw-bench/out/SCORECARD.md
#   - docs/SCORECARD.md
#
# Tuloste on reprodusoitava: kaksi peräkkäistä ajoa tuottaa byte-identtisen
# scorecard.json:n (design §6).
#
# Aja:  bash scripts/bench.sh
#
# HUOM: GNU-toolchain on rikki tällä koneella → käytä stable-MSVC:tä.

set -euo pipefail

TOOLCHAIN="${BENCH_TOOLCHAIN:-+stable-x86_64-pc-windows-msvc}"

echo "═══════════════════════════════════════════════════════════"
echo "  FamilyClaw Continuity Benchmark — reproducible proof"
echo "═══════════════════════════════════════════════════════════"

# 1) Rakenna musta laatikko (continuity_daemon) ENNEN ajoa — harness paikantaa
#    sen target/<profile>/-hakemistosta.
echo ">>> building continuity_daemon (black box) <<<"
cargo "$TOOLCHAIN" build -p familyclaw-agent --bin continuity_daemon

# 2) Aja kaikki skenaariot kiinteällä kellolla → scorecard.
echo ">>> running all scenarios <<<"
cargo "$TOOLCHAIN" run -p familyclaw-bench -- all

echo "═══════════════════════════════════════════════════════════"
echo "  ✅ scorecard written to crates/familyclaw-bench/out/ + docs/"
echo "═══════════════════════════════════════════════════════════"
