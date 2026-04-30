//! Single-threaded throughput bench for engine v1 (option B) vs engine v2
//! (option A). See task 07a in `~/payments-engine-challenge-docs/`.
//!
//! Compiled only with `--features bench`; the body is feature-gated so a
//! plain `cargo build` skips it. `criterion_main!` must sit at the crate
//! root, so the feature-off shim provides its own `main`.

#[cfg(feature = "bench")]
use std::time::{Duration, Instant};

#[cfg(feature = "bench")]
use criterion::{Criterion, criterion_group, criterion_main};

#[cfg(feature = "bench")]
use payments_engine_challenge::bench_support::{TX_THROUGHPUT, generate_default};
#[cfg(feature = "bench")]
use payments_engine_challenge::engine::v1;
#[cfg(feature = "bench")]
use payments_engine_challenge::engine::v1::account::Account;
#[cfg(feature = "bench")]
use payments_engine_challenge::engine::v1::transaction::Transaction;
#[cfg(feature = "bench")]
use payments_engine_challenge::engine::v2;

#[cfg(feature = "bench")]
fn run_v1(workload: &[Transaction]) -> Vec<Account> {
    let mut engine = v1::Engine::new();
    for tx in workload {
        let _ = engine.process(tx.clone());
    }
    let mut snapshot: Vec<_> = engine.accounts().cloned().collect();
    snapshot.sort_by_key(Account::client);
    snapshot
}

#[cfg(feature = "bench")]
fn run_v2(workload: &[Transaction]) -> Vec<Account> {
    let mut engine = v2::Engine::new();
    for tx in workload {
        let _ = engine.process(tx.clone());
    }
    let mut snapshot: Vec<_> = engine.accounts().cloned().collect();
    snapshot.sort_by_key(Account::client);
    snapshot
}

#[cfg(feature = "bench")]
fn throughput(c: &mut Criterion) {
    let workload = generate_default(TX_THROUGHPUT);

    // Correctness gate: under the same input, both engines must produce
    // identical final account state. A mismatch invalidates the timed
    // comparison, so we abort before any measurement.
    let v1_state = run_v1(&workload);
    let v2_state = run_v2(&workload);
    assert_eq!(
        v1_state, v2_state,
        "engine v1 and v2 disagreed on the synthetic workload",
    );

    let mut group = c.benchmark_group("throughput_1m");
    group.throughput(criterion::Throughput::Elements(workload.len() as u64));

    group.bench_function("v1_option_b", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut engine = v1::Engine::new();
                let start = Instant::now();
                for tx in &workload {
                    let _ = engine.process(tx.clone());
                }
                total += start.elapsed();
            }
            total
        });
    });

    group.bench_function("v2_option_a", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let mut engine = v2::Engine::new();
                let start = Instant::now();
                for tx in &workload {
                    let _ = engine.process(tx.clone());
                }
                total += start.elapsed();
            }
            total
        });
    });

    group.finish();
}

#[cfg(feature = "bench")]
criterion_group!(benches, throughput);
#[cfg(feature = "bench")]
criterion_main!(benches);

#[cfg(not(feature = "bench"))]
fn main() {
    eprintln!("rebuild with --features bench to run this benchmark");
}
