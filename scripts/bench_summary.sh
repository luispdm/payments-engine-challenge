#!/usr/bin/env bash
# Run the concurrency-variant benches and emit a markdown summary table
# for `docs/concurrency-benchmarks.md`.
#
# Usage:
#   scripts/bench_summary.sh             run criterion + one-shot bins, print tables
#   scripts/bench_summary.sh --skip-bench reuse cached criterion output (one-shot
#                                        bins always rerun because they're cheap)
#
# Requires: cargo, jq, awk.

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

VARIANTS=(baseline mutex dashmap actor_std actor_crossbeam)
OVERLAPS=(0 25 50 100)
TX_BENCH=1000000
# Scaling sweep at 50% overlap: how throughput / latency scale with
# tx_count. Mirrors SCALING_TX_COUNTS in benches/throughput.rs.
SCALING_TX_COUNTS=(100 1000 10000 100000 1000000)
SCALING_OVERLAP_PCT=50

# Throughput bench. Criterion writes per-bench estimates.json under
# target/criterion/throughput_1m/<variant>/<ovK>/new/estimates.json.
if [[ $SKIP_BENCH -eq 0 ]]; then
    cargo bench --bench throughput --features bench >&2
fi

read_mean_ns() { jq -r '.mean.point_estimate' "$1"; }
read_stddev_ns() { jq -r '.std_dev.point_estimate' "$1"; }
fmt_ms() { awk -v ns="$1" 'BEGIN { printf "%.2f", ns/1e6 }'; }
fmt_throughput() {
    awk -v ns="$1" -v tx="$2" 'BEGIN { printf "%.2f", tx/(ns/1e9)/1e6 }'
}
fmt_mib() { awk -v kb="$1" 'BEGIN { printf "%.1f", kb/1024 }'; }
fmt_us() { awk -v ns="$1" 'BEGIN { printf "%.1f", ns/1e3 }'; }

# Build the one-shot bench bins once and run each per overlap ratio.
cargo build --release --features bench \
    --bin bench_baseline --bin bench_mutex --bin bench_dashmap \
    --bin bench_actor_std --bin bench_actor_crossbeam >&2

# Pull a single key=value token out of a one-shot bin's stdout line.
extract_kv() {
    awk -v line="$1" -v key="$2" 'BEGIN {
        n = split(line, kv, " ")
        for (i = 1; i <= n; i++) {
            split(kv[i], pair, "=")
            if (pair[1] == key) print pair[2]
        }
    }'
}

declare -A RSS_KB
declare -A P50_NS
declare -A P90_NS
declare -A P99_NS

for v in "${VARIANTS[@]}"; do
    for o in "${OVERLAPS[@]}"; do
        line="$(BENCH_OVERLAP_PCT="$o" "./target/release/bench_${v}")"
        # variant=NAME overlap=N tx=N accounts=N elapsed_ns=N peak_rss_kb=N p50_ns=N p90_ns=N p99_ns=N
        RSS_KB["${v}_${o}"]=$(extract_kv "$line" "peak_rss_kb")
        P50_NS["${v}_${o}"]=$(extract_kv "$line" "p50_ns")
        P90_NS["${v}_${o}"]=$(extract_kv "$line" "p90_ns")
        P99_NS["${v}_${o}"]=$(extract_kv "$line" "p99_ns")
    done
done

# Throughput table per overlap ratio.
echo
echo "## Throughput (1M tx, mean ± stddev in ms; throughput in Mtx/s)"
echo
header="| Variant |"
for o in "${OVERLAPS[@]}"; do
    header="$header ov${o}% (ms) | ov${o}% (Mtx/s) |"
done
echo "$header"
sep="|---------|"
for _ in "${OVERLAPS[@]}"; do
    sep="$sep------------|---------------|"
done
echo "$sep"
for v in "${VARIANTS[@]}"; do
    row="| $v |"
    for o in "${OVERLAPS[@]}"; do
        json="target/criterion/throughput_1m/${v}/ov${o}/new/estimates.json"
        if [[ -f "$json" ]]; then
            mean_ns=$(read_mean_ns "$json")
            stddev_ns=$(read_stddev_ns "$json")
            mean_ms=$(fmt_ms "$mean_ns")
            stddev_ms=$(fmt_ms "$stddev_ns")
            thru=$(fmt_throughput "$mean_ns" "$TX_BENCH")
            row="$row $mean_ms ± $stddev_ms | $thru |"
        else
            row="$row n/a | n/a |"
        fi
    done
    echo "$row"
done

# One-shot bins: 10M-tx workload at every overlap ratio.
echo
echo "## Tail latency and peak RSS (10M tx, per overlap ratio)"
echo
echo "| Variant | Overlap | p50 (µs) | p90 (µs) | p99 (µs) | Peak RSS (MiB) |"
echo "|---------|---------|----------|----------|----------|----------------|"
for v in "${VARIANTS[@]}"; do
    for o in "${OVERLAPS[@]}"; do
        rss_kb="${RSS_KB[${v}_${o}]}"
        p50="${P50_NS[${v}_${o}]}"
        p90="${P90_NS[${v}_${o}]}"
        p99="${P99_NS[${v}_${o}]}"
        rss_mib=$(fmt_mib "$rss_kb")
        p50_us=$(fmt_us "$p50")
        p90_us=$(fmt_us "$p90")
        p99_us=$(fmt_us "$p99")
        echo "| $v | ${o}% | $p50_us | $p90_us | $p99_us | $rss_mib |"
    done
done

# Scaling sweep latency: per (variant, tx_count) at 50% overlap. RSS is
# omitted because at small tx counts it just reflects bin startup pages.
declare -A SCALE_P50_NS
declare -A SCALE_P90_NS
declare -A SCALE_P99_NS

for v in "${VARIANTS[@]}"; do
    for n in "${SCALING_TX_COUNTS[@]}"; do
        line="$(BENCH_OVERLAP_PCT="$SCALING_OVERLAP_PCT" BENCH_TX_COUNT="$n" "./target/release/bench_${v}")"
        SCALE_P50_NS["${v}_${n}"]=$(extract_kv "$line" "p50_ns")
        SCALE_P90_NS["${v}_${n}"]=$(extract_kv "$line" "p90_ns")
        SCALE_P99_NS["${v}_${n}"]=$(extract_kv "$line" "p99_ns")
    done
done

echo
echo "## Scaling sweep at 50% overlap (throughput + tail latency)"
echo
echo "| Variant | tx_count | Throughput (Mtx/s) | Iter (ms) | p50 (µs) | p90 (µs) | p99 (µs) |"
echo "|---------|----------|--------------------|-----------|----------|----------|----------|"
for v in "${VARIANTS[@]}"; do
    for n in "${SCALING_TX_COUNTS[@]}"; do
        json="target/criterion/scaling_50ov/${v}/tx${n}/new/estimates.json"
        if [[ -f "$json" ]]; then
            mean_ns=$(read_mean_ns "$json")
            mean_ms=$(fmt_ms "$mean_ns")
            thru=$(fmt_throughput "$mean_ns" "$n")
        else
            mean_ms="n/a"
            thru="n/a"
        fi
        p50=$(fmt_us "${SCALE_P50_NS[${v}_${n}]}")
        p90=$(fmt_us "${SCALE_P90_NS[${v}_${n}]}")
        p99=$(fmt_us "${SCALE_P99_NS[${v}_${n}]}")
        echo "| $v | $n | $thru | $mean_ms | $p50 | $p90 | $p99 |"
    done
done
