//! One-shot memory measurement binary for engine v1.
//!
//! Drives the synthetic workload through the engine, then reads peak
//! resident set size via `getrusage(RUSAGE_SELF)` and prints it on stdout
//! so `scripts/bench_summary.sh` can scrape the value.

#[cfg(feature = "bench")]
fn main() {
    use payments_engine_challenge::bench_support::{TX_MEMORY, generate_default};
    use payments_engine_challenge::engine::v1::Engine;
    use payments_engine_challenge::mem::peak_rss_kb;

    let workload = generate_default(TX_MEMORY);
    let mut engine = Engine::new();
    for tx in workload {
        let _ = engine.process(tx);
    }
    let accounts: usize = engine.accounts().count();

    let rss_kb = peak_rss_kb();
    println!("variant=v1 tx={TX_MEMORY} accounts={accounts} peak_rss_kb={rss_kb}");
}

#[cfg(not(feature = "bench"))]
fn main() {
    eprintln!("rebuild with --features bench to run this binary");
    std::process::exit(2);
}
