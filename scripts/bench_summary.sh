#!/usr/bin/env bash
# Run the data-structure benches and emit a markdown summary table for
# `docs/data-structures-benchmarks.md`.
#
# Usage:
#   scripts/bench_summary.sh             run benches + memory bins, print table
#   scripts/bench_summary.sh --skip-bench skip the criterion run; reuse
#                                        whatever results live under
#                                        target/criterion (memory bins
#                                        always run because they're cheap)
#
# Requires: cargo, jq.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

SKIP_BENCH=0
if [[ "${1:-}" == "--skip-bench" ]]; then
    SKIP_BENCH=1
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "error: jq not found on PATH" >&2
    exit 1
fi

# Throughput bench. Criterion writes per-bench estimates.json under
# target/criterion. We sample the latest run.
if [[ $SKIP_BENCH -eq 0 ]]; then
    cargo bench --bench throughput --features bench >&2
fi

ESTIMATES_DIR="target/criterion/throughput_1m"
V1_JSON="$ESTIMATES_DIR/v1/new/estimates.json"
V2_JSON="$ESTIMATES_DIR/v2/new/estimates.json"

if [[ ! -f "$V1_JSON" || ! -f "$V2_JSON" ]]; then
    echo "error: criterion estimates not found; run without --skip-bench first" >&2
    exit 1
fi

# estimates.json carries mean and std_dev in nanoseconds for one iteration
# of the bench (= one full pass over the 1M-tx workload).
TX_THROUGHPUT=1000000

read_mean_ns() { jq -r '.mean.point_estimate' "$1"; }
read_stddev_ns() { jq -r '.std_dev.point_estimate' "$1"; }

V1_MEAN_NS=$(read_mean_ns "$V1_JSON")
V1_STDDEV_NS=$(read_stddev_ns "$V1_JSON")
V2_MEAN_NS=$(read_mean_ns "$V2_JSON")
V2_STDDEV_NS=$(read_stddev_ns "$V2_JSON")

# Memory bench: build then run each bin. Output line shape is
# `variant=vX tx=N accounts=N peak_rss_kb=K`.
cargo build --release --bin mem_v1 --bin mem_v2 --features bench >&2

V1_RSS_KB=$(./target/release/mem_v1 | awk -F'peak_rss_kb=' '{print $2}')
V2_RSS_KB=$(./target/release/mem_v2 | awk -F'peak_rss_kb=' '{print $2}')

# Format helpers. Throughput in Melem/s, time in ms, RSS in MiB.
fmt_ms() { awk -v ns="$1" 'BEGIN { printf "%.2f", ns/1e6 }'; }
fmt_throughput() {
    awk -v ns="$1" -v tx="$2" 'BEGIN { printf "%.2f", tx/(ns/1e9)/1e6 }'
}
fmt_mib() { awk -v kb="$1" 'BEGIN { printf "%.1f", kb/1024 }'; }

V1_TIME_MS=$(fmt_ms "$V1_MEAN_NS")
V1_TIME_STDDEV_MS=$(fmt_ms "$V1_STDDEV_NS")
V2_TIME_MS=$(fmt_ms "$V2_MEAN_NS")
V2_TIME_STDDEV_MS=$(fmt_ms "$V2_STDDEV_NS")

V1_THRU=$(fmt_throughput "$V1_MEAN_NS" "$TX_THROUGHPUT")
V2_THRU=$(fmt_throughput "$V2_MEAN_NS" "$TX_THROUGHPUT")

V1_MIB=$(fmt_mib "$V1_RSS_KB")
V2_MIB=$(fmt_mib "$V2_RSS_KB")

cat <<TABLE
| Variant | Storage | 1M-tx mean ± stddev (ms) | Throughput (Mtx/s) | 10M-tx peak RSS (MiB) |
|---------|---------|--------------------------|--------------------|-----------------------|
| v1      | single \`HashMap<u32, TxRecord>\` | $V1_TIME_MS ± $V1_TIME_STDDEV_MS | $V1_THRU | $V1_MIB |
| v2      | \`HashMap<u32, DepositRecord>\` + \`HashSet<u32>\` | $V2_TIME_MS ± $V2_TIME_STDDEV_MS | $V2_THRU | $V2_MIB |
TABLE
