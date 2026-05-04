//! Concurrency-variant throughput bench.
//!
//! Sweeps four overlap ratios per variant; the multi-producer drivers
//! split the workload across 8 producer threads and feed each one a
//! deterministic per-stream slice of the synthetic workload. Engine
//! correctness is gated at 0% overlap (where the cross-variant final
//! state must match the single-threaded baseline byte-for-byte); higher
//! overlap ratios skip the strict equality check because chargebacks on
//! shared clients are order-sensitive (documented in
//! `docs/concurrency-benchmarks.md`).
//!
//! Compiled only with `--features bench`; the body is feature-gated so a
//! plain `cargo build` skips it.

#[cfg(feature = "bench")]
use std::time::{Duration, Instant};

#[cfg(feature = "bench")]
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

#[cfg(feature = "bench")]
use payments_engine_challenge::bench_support::{
    PRODUCERS, ProducerStream, StreamWorkloadParams, TX_BENCH, generate_streams,
};
#[cfg(feature = "bench")]
use payments_engine_challenge::concurrency::{
    actor_crossbeam, actor_std, baseline, dashmap_engine, mutex,
};
#[cfg(feature = "bench")]
use payments_engine_challenge::engine::account::Account;

#[cfg(feature = "bench")]
const OVERLAPS: [u8; 4] = [0, 25, 50, 100];

/// Tx-count axis for the scaling sweep at fixed 50% overlap. The
/// scaling bench answers "how does each variant degrade or amortize
/// as the workload grows" — the existing throughput bench (overlap
/// sweep at 1M tx) holds tx_count constant and varies overlap
/// instead.
#[cfg(feature = "bench")]
const SCALING_TX_COUNTS: [usize; 5] = [100, 1_000, 10_000, 100_000, 1_000_000];

/// Fixed overlap ratio for the scaling sweep. 50% sits on the contention
/// curve between disjoint clients (0%) and full sharing (100%); it is
/// the most representative single point of case-B production traffic.
#[cfg(feature = "bench")]
const SCALING_OVERLAP_PCT: u8 = 50;

/// One variant's per-iteration runner. Each call constructs a fresh
/// engine inside the variant's drive function, so accumulated state
/// between criterion iterations doesn't distort the measurement.
#[cfg(feature = "bench")]
fn run_one_iter(variant: Variant, streams: Vec<ProducerStream>) {
    match variant {
        Variant::Baseline => {
            let _ = baseline::run_workload(1, streams);
        }
        Variant::Mutex => {
            let _ = mutex::run_workload(streams.len(), streams);
        }
        Variant::DashMap => {
            let _ = dashmap_engine::run_workload(streams.len(), streams);
        }
        Variant::ActorStd => {
            let _ = actor_std::run_workload(streams.len(), streams);
        }
        Variant::ActorCrossbeam => {
            let _ = actor_crossbeam::run_workload(streams.len(), streams);
        }
    }
}

#[cfg(feature = "bench")]
#[derive(Clone, Copy)]
enum Variant {
    Baseline,
    Mutex,
    DashMap,
    ActorStd,
    ActorCrossbeam,
}

#[cfg(feature = "bench")]
impl Variant {
    fn name(self) -> &'static str {
        match self {
            Variant::Baseline => "baseline",
            Variant::Mutex => "mutex",
            Variant::DashMap => "dashmap",
            Variant::ActorStd => "actor_std",
            Variant::ActorCrossbeam => "actor_crossbeam",
        }
    }
}

#[cfg(feature = "bench")]
fn variant_final_state(variant: Variant, streams: Vec<ProducerStream>) -> Vec<Account> {
    match variant {
        Variant::Baseline => baseline::run_workload(1, streams).accounts,
        Variant::Mutex => mutex::run_workload(streams.len(), streams).accounts,
        Variant::DashMap => dashmap_engine::run_workload(streams.len(), streams).accounts,
        Variant::ActorStd => actor_std::run_workload(streams.len(), streams).accounts,
        Variant::ActorCrossbeam => actor_crossbeam::run_workload(streams.len(), streams).accounts,
    }
}

#[cfg(feature = "bench")]
fn correctness_gate(streams_factory: impl Fn() -> Vec<ProducerStream>) {
    // 0%-overlap streams: every producer owns a disjoint slice of
    // clients, so the final state is invariant to interleaving and
    // every variant must match the baseline byte-for-byte.
    let baseline_state = variant_final_state(Variant::Baseline, streams_factory());
    for v in [
        Variant::Mutex,
        Variant::DashMap,
        Variant::ActorStd,
        Variant::ActorCrossbeam,
    ] {
        let state = variant_final_state(v, streams_factory());
        assert_eq!(
            state.len(),
            baseline_state.len(),
            "variant {} disagrees with baseline on account count",
            v.name(),
        );
        assert_eq!(
            state,
            baseline_state,
            "variant {} produced different final state than baseline at 0% overlap",
            v.name(),
        );
    }
}

#[cfg(feature = "bench")]
fn throughput(c: &mut Criterion) {
    // Run the cross-variant correctness gate once at 0% overlap before
    // any timed work. A mismatch aborts the bench.
    correctness_gate(|| {
        generate_streams(StreamWorkloadParams {
            tx_count: TX_BENCH,
            producers: PRODUCERS,
            overlap_pct: 0,
            ..Default::default()
        })
    });

    let mut group = c.benchmark_group("throughput_1m");
    group.throughput(Throughput::Elements(TX_BENCH as u64));
    // 1M-tx iters are heavyweight; cap sample size and warm-up.
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(15));

    for &overlap in &OVERLAPS {
        let streams_seed = StreamWorkloadParams {
            tx_count: TX_BENCH,
            producers: PRODUCERS,
            overlap_pct: overlap,
            ..Default::default()
        };
        // Pre-generate once so the timed region only measures the apply
        // work; clone per iteration.
        let template = generate_streams(streams_seed);

        for variant in [
            Variant::Baseline,
            Variant::Mutex,
            Variant::DashMap,
            Variant::ActorStd,
            Variant::ActorCrossbeam,
        ] {
            let id = BenchmarkId::new(variant.name(), format!("ov{overlap}"));
            let template = template.clone();
            group.bench_with_input(id, &template, |b, template| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let streams = template.clone();
                        let start = Instant::now();
                        run_one_iter(variant, streams);
                        total += start.elapsed();
                    }
                    total
                });
            });
        }
    }
    group.finish();
}

/// Scaling sweep at fixed 50% overlap. Each cell measures the wall
/// clock to drain a workload of `tx_count` transactions through the
/// variant; criterion takes care of per-iter scaling so even the
/// 100-tx cell collects enough iters for a meaningful estimate.
#[cfg(feature = "bench")]
fn scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("scaling_50ov");
    // Reuse the same harness budget as the throughput bench. Criterion
    // scales the per-sample iter count automatically; small tx_counts
    // pack many iters per sample, large ones pack few.
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(15));

    for &tx_count in &SCALING_TX_COUNTS {
        let streams_seed = StreamWorkloadParams {
            tx_count,
            producers: PRODUCERS,
            overlap_pct: SCALING_OVERLAP_PCT,
            ..Default::default()
        };
        let template = generate_streams(streams_seed);
        // Stamp Throughput::Elements per cell so criterion reports
        // the right Mtx/s for each tx_count.
        group.throughput(Throughput::Elements(tx_count as u64));

        for variant in [
            Variant::Baseline,
            Variant::Mutex,
            Variant::DashMap,
            Variant::ActorStd,
            Variant::ActorCrossbeam,
        ] {
            let id = BenchmarkId::new(variant.name(), format!("tx{tx_count}"));
            let template = template.clone();
            group.bench_with_input(id, &template, |b, template| {
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let streams = template.clone();
                        let start = Instant::now();
                        run_one_iter(variant, streams);
                        total += start.elapsed();
                    }
                    total
                });
            });
        }
    }
    group.finish();
}

#[cfg(feature = "bench")]
criterion_group!(benches, throughput, scaling);
#[cfg(feature = "bench")]
criterion_main!(benches);

#[cfg(not(feature = "bench"))]
fn main() {
    eprintln!("rebuild with --features bench to run this benchmark");
}
