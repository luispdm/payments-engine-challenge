//! One-shot bench bin for the single-threaded baseline.
//!
//! Reads `BENCH_OVERLAP_PCT` (default 100) and `BENCH_TX_COUNT`
//! (default 10M) from the environment so the summary script can
//! sweep either axis across one-shot runs.

#[cfg(feature = "bench")]
fn main() {
    use std::time::Instant;

    use payments_engine_challenge::bench_support::{
        OneShotReport, generate_streams, one_shot_overlap_from_env, one_shot_params,
        one_shot_tx_count_from_env, print_one_shot_report,
    };
    use payments_engine_challenge::concurrency::baseline;
    use payments_engine_challenge::concurrency::workload::{merge_recorders, percentiles};
    use payments_engine_challenge::mem::peak_rss_kb;

    let overlap_pct = one_shot_overlap_from_env();
    let tx_count = one_shot_tx_count_from_env();
    let params = one_shot_params(tx_count, overlap_pct);
    let streams = generate_streams(params);

    let start = Instant::now();
    let result = baseline::run_workload(1, streams);
    let elapsed = start.elapsed();

    let merged = merge_recorders(&result.recorders);
    let (p50_ns, p90_ns, p99_ns) = percentiles(&merged);
    print_one_shot_report(&OneShotReport {
        variant: "baseline",
        overlap_pct,
        tx_count: params.tx_count,
        accounts: result.accounts.len(),
        elapsed,
        peak_rss_kb: peak_rss_kb(),
        p50_ns,
        p90_ns,
        p99_ns,
    });
}

#[cfg(not(feature = "bench"))]
fn main() {
    eprintln!("rebuild with --features bench to run this binary");
    std::process::exit(2);
}
