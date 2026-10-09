#!/usr/bin/env bash
# Reproducible benchmark run. Prints system info and the full suite.
# Usage: scripts/bench.sh [real-directory] [records] [iterations]
set -euo pipefail
cd "$(dirname "$0")/.."
DIR="${1:-crates}"
RECORDS="${2:-1000000}"
ITER="${3:-100}"
cargo build -q --release -p everyfiles-cli
echo "## system"
uname -srm 2>/dev/null || ver
if command -v sysctl >/dev/null 2>&1 && sysctl -n machdep.cpu.brand_string >/dev/null 2>&1; then
  echo "cpu: $(sysctl -n machdep.cpu.brand_string)"
elif [ -r /proc/cpuinfo ]; then
  echo "cpu: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | sed 's/^ //')"
fi
echo "rustc: $(rustc --version)"
echo
# warm the page cache so scans measure the enumeration path, not the disk
find "$DIR" -type f > /dev/null 2>&1 || true
target/release/everyfiles bench --records "$RECORDS" --iterations "$ITER" --path "$DIR" --scan-iterations 3
