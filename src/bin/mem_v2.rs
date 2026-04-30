//! One-shot memory measurement binary for engine v2.
//!
//! Mirror of `mem_v1` so `scripts/bench_summary.sh` can compare peak RSS
//! between the two storage layouts under identical input.

#[cfg(feature = "bench")]
fn main() {
    use payments_engine_challenge::bench_support::{TX_MEMORY, generate_default};
    use payments_engine_challenge::engine::v2::Engine;
    use payments_engine_challenge::mem::peak_rss_kb;

    let workload = generate_default(TX_MEMORY);
    let mut engine = Engine::new();
    for tx in workload {
        let _ = engine.process(tx);
    }
    let accounts: usize = engine.accounts().count();

    let rss_kb = peak_rss_kb();
    println!("variant=v2 tx={TX_MEMORY} accounts={accounts} peak_rss_kb={rss_kb}");
}

#[cfg(not(feature = "bench"))]
fn main() {
    eprintln!("rebuild with --features bench to run this binary");
    std::process::exit(2);
}
