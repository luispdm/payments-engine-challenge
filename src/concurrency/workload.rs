//! Shared driver utilities for the concurrency variants.
//!
//! Defines the latency-recording struct each variant feeds, the workload
//! result that bench drivers return, and a small helper to turn an
//! account iterator into a deterministic snapshot.

use std::time::Instant;

use hdrhistogram::Histogram;

use crate::engine::account::Account;
use crate::engine::transaction::Transaction;

/// Channel envelope used by the actor variants.
///
/// Producers stamp `sent_at_ns` (offset from the bench's `Instant`)
/// immediately before they hand the envelope to the channel. The
/// consumer reads the stamp after [`crate::engine::Engine::process`]
/// returns and records `now - sent_at_ns` so the histogram captures
/// queue-wait + apply latency, not just channel-enqueue cost. The
/// sync variants (baseline, mutex, dashmap) keep producer-side
/// timing because their `submit` is synchronous.
pub struct BenchEnvelope {
    /// The transaction the engine will apply.
    pub tx: Transaction,
    /// Producer-stamped offset from the bench's start `Instant`.
    pub sent_at_ns: u64,
}

/// Per-producer apply-latency recorder.
///
/// Records each tx's "applied at the engine" minus "stamped by the
/// producer" delta in nanoseconds. The histogram tracks values up to
/// roughly 60 seconds with 3 significant-digit precision; latencies past
/// that are saturated rather than aborting the run.
pub struct LatencyRecorder {
    histogram: Histogram<u64>,
}

impl Default for LatencyRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyRecorder {
    /// Construct a new recorder with the bench's standard precision and
    /// range. Panics only on programmer error in the
    /// `Histogram::new_with_bounds` arguments.
    pub fn new() -> Self {
        let histogram = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
            .expect("hdrhistogram bounds are valid by construction");
        Self { histogram }
    }

    /// Record the latency in nanoseconds between `bench_start` (set on
    /// the consumer side at workload entry) and `sent_at_ns` (offset
    /// from `bench_start` stamped by the producer when the tx left its
    /// hand). The current instant is sampled inside the recorder so the
    /// caller doesn't pay an `Instant::now()` outside the timed loop.
    pub fn record(&mut self, bench_start: Instant, sent_at_ns: u64) {
        let now_ns = bench_start.elapsed().as_nanos() as u64;
        let latency = now_ns.saturating_sub(sent_at_ns);
        // Saturate-record so a hot-cache outlier never crashes the bench.
        let _ = self
            .histogram
            .record(latency.clamp(1, self.histogram.high()));
    }

    /// Borrow the underlying histogram. Used by aggregators in the
    /// one-shot bench bins.
    pub fn histogram(&self) -> &Histogram<u64> {
        &self.histogram
    }
}

/// Result of one bench `run_workload` call.
pub struct WorkloadResult {
    /// Deterministic, client-sorted snapshot of every known account.
    pub accounts: Vec<Account>,
    /// Per-producer latency recorders. Baseline returns one;
    /// multi-producer variants return one per producer thread.
    pub recorders: Vec<LatencyRecorder>,
}

/// Collect an account iterator into a deterministic, client-sorted
/// snapshot. All variants funnel through this helper so the cross-variant
/// correctness gate compares apples to apples.
pub fn sorted_accounts<I>(accounts: I) -> Vec<Account>
where
    I: IntoIterator<Item = Account>,
{
    let mut out: Vec<Account> = accounts.into_iter().collect();
    out.sort_by_key(Account::client);
    out
}

/// Merge per-producer recorders into one histogram. Used by the
/// one-shot bench bins to emit a single set of percentiles per variant.
#[must_use]
pub fn merge_recorders(recorders: &[LatencyRecorder]) -> Histogram<u64> {
    let mut merged = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
        .expect("hdrhistogram bounds are valid by construction");
    for r in recorders {
        merged
            .add(r.histogram())
            .expect("merging compatible histograms cannot fail");
    }
    merged
}

/// Triple of `(p50, p90, p99)` in nanoseconds, extracted from `hist`.
#[must_use]
pub fn percentiles(hist: &Histogram<u64>) -> (u64, u64, u64) {
    (
        hist.value_at_quantile(0.50),
        hist.value_at_quantile(0.90),
        hist.value_at_quantile(0.99),
    )
}
