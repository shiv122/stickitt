#!/usr/bin/env sh
# Measures Stick-itt's footprint with N notes open (macOS/Linux).
# Usage: scripts/measure.sh [counts...]   (default: 1 10 30)
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
exe="$root/target/release/stickit"
[ -x "$exe" ] || (cd "$root" && cargo build --release)
idle=10

printf "%-6s %-10s %-12s\n" Notes "RSS MB" "Idle CPU s"
for n in ${@:-1 10 30}; do
    "$exe" --bench "$n" &
    pid=$!
    sleep 3
    t0=$(ps -o cputime= -p "$pid" | awk -F: '{ print ($1 * 60) + $2 }')
    sleep "$idle"
    t1=$(ps -o cputime= -p "$pid" | awk -F: '{ print ($1 * 60) + $2 }')
    rss=$(ps -o rss= -p "$pid")
    printf "%-6s %-10.1f %-12s\n" "$n" "$(echo "$rss / 1024" | bc -l)" "$(echo "$t1 - $t0" | bc -l)"
    kill "$pid"
    sleep 0.5
done
