//! Synthetic workload generator for the engine benchmarks (07b reuses
//! this). Output is deterministic for a given `(seed, clients, tx_count)`
//! triple so successive runs of the same harness are directly comparable.
//!
//! Gated behind the `bench` feature: production builds carry no `rand`
//! dependency.

use rand::{RngExt, SeedableRng, rngs::SmallRng};
use rust_decimal::Decimal;

use crate::engine::transaction::Transaction;

/// Default seed for the bundled workloads. Pinning a constant means the
/// throughput and memory bins are reproducible across runs and machines.
pub const DEFAULT_SEED: u64 = 0x0000_7ABE_5EED;

/// Default client cardinality. 10k matches the task spec; large enough to
/// stress hashmap collisions, small enough to keep per-client state tiny.
pub const CLIENTS: u16 = 10_000;

/// Throughput bench size (criterion).
pub const TX_THROUGHPUT: usize = 1_000_000;

/// Memory bench size (one-shot bin).
pub const TX_MEMORY: usize = 10_000_000;

/// Knobs the generator runs against. Constructors wrap [`Default`] so most
/// call sites just override one field via the struct-update syntax.
#[derive(Debug, Clone, Copy)]
pub struct WorkloadParams {
    /// Total tx count emitted.
    pub tx_count: usize,
    /// Number of distinct client ids the generator may pick from.
    pub clients: u16,
    /// Seed for the deterministic SmallRng.
    pub seed: u64,
}

impl Default for WorkloadParams {
    fn default() -> Self {
        Self {
            tx_count: TX_THROUGHPUT,
            clients: CLIENTS,
            seed: DEFAULT_SEED,
        }
    }
}

/// Generate a deterministic workload of `tx_count` transactions using the
/// default client cardinality and seed.
#[must_use]
pub fn generate_default(tx_count: usize) -> Vec<Transaction> {
    generate(WorkloadParams {
        tx_count,
        ..Default::default()
    })
}

/// Generate a deterministic workload matching the task 07a tx mix.
///
/// Mix: 50% deposit, 30% withdrawal, 10% dispute, 7% resolve, 3%
/// chargeback. Lifecycle events (dispute/resolve/chargeback) target a
/// uniformly random already-emitted deposit; if none exists yet the slot
/// is filled with a deposit instead so the workload starts up cleanly.
/// The generator does not track dispute state, so a fraction of resolves
/// and chargebacks land on undisputed deposits and exercise the
/// `NotDisputed` error path. That matches the realistic stream a partner
/// would emit.
#[must_use]
pub fn generate(params: WorkloadParams) -> Vec<Transaction> {
    let WorkloadParams {
        tx_count,
        clients,
        seed,
    } = params;
    assert!(clients > 0, "clients must be positive");

    let mut rng = SmallRng::seed_from_u64(seed);
    let mut workload = Vec::with_capacity(tx_count);
    // Track tx ids of already-emitted deposits so dispute lifecycle events
    // have legitimate targets to point at. A `Vec` (rather than `HashSet`)
    // because we only ever sample uniformly; deposits never leave the pool.
    let mut deposit_pool: Vec<(u16, u32)> = Vec::with_capacity(tx_count / 2);
    let mut next_tx: u32 = 1;

    for _ in 0..tx_count {
        let pick = rng.random_range(0u32..100);
        let tx = match pick {
            0..50 => {
                let client = rng.random_range(0..clients);
                let tx_id = next_tx;
                next_tx += 1;
                deposit_pool.push((client, tx_id));
                Transaction::Deposit {
                    client,
                    tx: tx_id,
                    amount: random_amount(&mut rng),
                }
            }
            50..80 => {
                let client = rng.random_range(0..clients);
                let tx_id = next_tx;
                next_tx += 1;
                Transaction::Withdrawal {
                    client,
                    tx: tx_id,
                    amount: random_amount(&mut rng),
                }
            }
            kind => {
                // Dispute / resolve / chargeback. Sample a real deposit if
                // any exists; otherwise emit a deposit so the workload
                // does not start with a long ignorable-error prefix.
                let Some(&(client, tx_id)) = sample(&mut rng, &deposit_pool) else {
                    let client = rng.random_range(0..clients);
                    let tx_id = next_tx;
                    next_tx += 1;
                    deposit_pool.push((client, tx_id));
                    workload.push(Transaction::Deposit {
                        client,
                        tx: tx_id,
                        amount: random_amount(&mut rng),
                    });
                    continue;
                };
                match kind {
                    80..90 => Transaction::Dispute { client, tx: tx_id },
                    90..97 => Transaction::Resolve { client, tx: tx_id },
                    _ => Transaction::Chargeback { client, tx: tx_id },
                }
            }
        };
        workload.push(tx);
    }
    workload
}

/// Random four-decimal-place amount in `(0, 1000]`. The lower bound is
/// strictly positive so generated rows never trip `NonPositiveAmount`
/// (task 06); within that window the engines exercise the success and
/// `InsufficientFunds` paths in roughly the expected ratios.
fn random_amount(rng: &mut SmallRng) -> Decimal {
    // 1..=10_000_000 in scale-4 units → 0.0001..=1000.0000.
    let units = rng.random_range(1u64..=10_000_000);
    Decimal::new(units as i64, 4)
}

fn sample<'a, T>(rng: &mut SmallRng, pool: &'a [T]) -> Option<&'a T> {
    if pool.is_empty() {
        None
    } else {
        Some(&pool[rng.random_range(0..pool.len())])
    }
}

/// Number of producer threads used by the multi-producer bench drivers.
/// Pinned per task 07b's deliverables.
pub const PRODUCERS: usize = 8;

/// Bounded-channel capacity for the actor variants.
pub const CHANNEL_CAPACITY: usize = 1024;

/// Default tx-count for the criterion throughput bench's per-iteration
/// pre-built workload (1M as in 07a).
pub const TX_BENCH: usize = TX_THROUGHPUT;

/// Per-producer slice of the synthetic workload.
///
/// The multi-producer bench drivers receive a `Vec<ProducerStream>` and
/// hand each entry to one producer thread. Within each stream the order
/// preserves the per-stream lifecycle invariant (`dispute` follows its
/// `deposit`); across streams the workload is composed so the final
/// account state matches the single-threaded baseline at 0% overlap.
/// Higher overlap ratios cross-pollinate clients between streams; that
/// trades the strict cross-variant equality gate for a
/// case-B-realistic workload.
#[derive(Debug, Clone, Default)]
pub struct ProducerStream {
    /// Transactions destined for this producer's thread, in order.
    pub txs: Vec<Transaction>,
}

/// Knobs for the multi-stream generator. Constructors wrap [`Default`].
#[derive(Debug, Clone, Copy)]
pub struct StreamWorkloadParams {
    /// Total tx count summed across all producer streams.
    pub tx_count: usize,
    /// Producer stream count (matches the bench driver's thread count).
    pub producers: usize,
    /// Total client-id cardinality (split across the streams' private
    /// and shared pools per `overlap_pct`).
    pub clients: u16,
    /// Client-pool overlap ratio in percent. `0` means each producer
    /// owns a disjoint client partition; `100` means every producer can
    /// touch every client.
    pub overlap_pct: u8,
    /// Seed for the deterministic SmallRng.
    pub seed: u64,
}

impl Default for StreamWorkloadParams {
    fn default() -> Self {
        Self {
            tx_count: TX_THROUGHPUT,
            producers: PRODUCERS,
            clients: CLIENTS,
            overlap_pct: 100,
            seed: DEFAULT_SEED,
        }
    }
}

/// Generate a deterministic per-producer workload split.
///
/// Each producer's stream is self-contained: lifecycle events fired by a
/// stream only target deposits the same stream emitted earlier, so the
/// per-stream order matches what the single-threaded engine would see
/// for that stream alone. Cross-stream ordering is up to the bench
/// runtime.
///
/// `overlap_pct == 0`: each producer is bucketed onto a disjoint slice
/// of `clients` so the engines can be cross-checked for strict
/// equality.
///
/// `overlap_pct > 0`: a fraction of every stream's draws come from a
/// shared client pool. Strict cross-variant equality is no longer
/// guaranteed because chargebacks on shared accounts are
/// order-sensitive; downstream consumers should rely on the 0% case for
/// the gate and on aggregate invariants (e.g. total-balance
/// preservation) for the rest.
#[must_use]
pub fn generate_streams(params: StreamWorkloadParams) -> Vec<ProducerStream> {
    let StreamWorkloadParams {
        tx_count,
        producers,
        clients,
        overlap_pct,
        seed,
    } = params;
    assert!(producers > 0, "producers must be positive");
    assert!(clients > 0, "clients must be positive");
    assert!(overlap_pct <= 100, "overlap_pct must be <= 100");

    // Carve `clients` into private (per-producer) and shared partitions.
    // shared = ceil(clients * overlap_pct / 100); the rest is split
    // evenly across producers as private buckets.
    let shared_count = ((u32::from(clients) * u32::from(overlap_pct)) / 100) as u16;
    let shared_count = shared_count.min(clients);
    let private_total = clients - shared_count;
    let private_per_producer = private_total / producers as u16;

    let mut rng = SmallRng::seed_from_u64(seed);
    let mut streams: Vec<ProducerStream> = (0..producers)
        .map(|_| ProducerStream {
            txs: Vec::with_capacity(tx_count / producers + 1),
        })
        .collect();

    // Per-producer deposit pools so lifecycle events stay self-targeted.
    // Each entry stores `(tx_id, client)` so dispute / resolve /
    // chargeback can address the same client the deposit was for
    // without scanning the producer's whole tx history (the O(N²)
    // version of this loop hung at 10M-tx workloads).
    let mut deposit_pools: Vec<Vec<(u32, u16)>> = (0..producers).map(|_| Vec::new()).collect();
    let mut next_tx: u32 = 1;

    let per_producer = tx_count / producers;
    let leftover = tx_count - per_producer * producers;

    for p in 0..producers {
        let target = per_producer + usize::from(p < leftover);
        let private_lo = shared_count + private_per_producer * p as u16;
        let private_hi = private_lo + private_per_producer;
        let stream = &mut streams[p];

        for _ in 0..target {
            let pick = rng.random_range(0u32..100);
            // Choose this draw's client. With probability overlap_pct
            // pull from the shared pool; otherwise from this producer's
            // private partition. If a partition is empty (e.g. 100%
            // overlap collapses the private partition to 0), fall
            // through to the other one.
            let use_shared = rng.random_range(0u32..100) < u32::from(overlap_pct);
            let client = if use_shared && shared_count > 0 {
                rng.random_range(0..shared_count)
            } else if private_hi > private_lo {
                rng.random_range(private_lo..private_hi)
            } else if shared_count > 0 {
                rng.random_range(0..shared_count)
            } else {
                // No clients available at all — degenerate input.
                0
            };

            let tx = match pick {
                0..50 => {
                    let tx_id = next_tx;
                    next_tx += 1;
                    deposit_pools[p].push((tx_id, client));
                    Transaction::Deposit {
                        client,
                        tx: tx_id,
                        amount: random_amount(&mut rng),
                    }
                }
                50..80 => {
                    let tx_id = next_tx;
                    next_tx += 1;
                    Transaction::Withdrawal {
                        client,
                        tx: tx_id,
                        amount: random_amount(&mut rng),
                    }
                }
                kind => {
                    // Lifecycle event: target one of THIS stream's
                    // earlier deposits (preserves per-stream
                    // deposit-before-dispute ordering). Falls back to a
                    // deposit if the stream's pool is empty.
                    let Some(&(target_tx, target_client)) = sample(&mut rng, &deposit_pools[p])
                    else {
                        let tx_id = next_tx;
                        next_tx += 1;
                        deposit_pools[p].push((tx_id, client));
                        stream.txs.push(Transaction::Deposit {
                            client,
                            tx: tx_id,
                            amount: random_amount(&mut rng),
                        });
                        continue;
                    };
                    match kind {
                        80..90 => Transaction::Dispute {
                            client: target_client,
                            tx: target_tx,
                        },
                        90..97 => Transaction::Resolve {
                            client: target_client,
                            tx: target_tx,
                        },
                        _ => Transaction::Chargeback {
                            client: target_client,
                            tx: target_tx,
                        },
                    }
                }
            };
            stream.txs.push(tx);
        }
    }

    streams
}

/// Workload parameters used by every per-variant one-shot bench
/// binary at a given `(tx_count, overlap_pct)` cell. Bins read
/// `BENCH_TX_COUNT` and `BENCH_OVERLAP_PCT` from the environment so
/// the summary script can sweep either axis without rebuilding.
#[must_use]
pub fn one_shot_params(tx_count: usize, overlap_pct: u8) -> StreamWorkloadParams {
    assert!(tx_count > 0, "tx_count must be positive");
    assert!(overlap_pct <= 100, "overlap_pct must be <= 100");
    StreamWorkloadParams {
        tx_count,
        producers: PRODUCERS,
        overlap_pct,
        ..Default::default()
    }
}

/// Read `BENCH_OVERLAP_PCT` from the environment, default to 100 if
/// unset or unparseable. Centralized so every bench bin sees the same
/// fallback semantics.
#[must_use]
pub fn one_shot_overlap_from_env() -> u8 {
    std::env::var("BENCH_OVERLAP_PCT")
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        .filter(|&v| v <= 100)
        .unwrap_or(100)
}

/// Read `BENCH_TX_COUNT` from the environment, default to
/// [`TX_MEMORY`] if unset or unparseable. Lets the summary script
/// sweep the tx-count axis through the per-variant one-shot bins
/// without changing the bins' default behavior.
#[must_use]
pub fn one_shot_tx_count_from_env() -> usize {
    std::env::var("BENCH_TX_COUNT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&v| v > 0)
        .unwrap_or(TX_MEMORY)
}

/// Snapshot of a one-shot bench bin's headline metrics.
pub struct OneShotReport<'a> {
    /// Variant name (`baseline`, `mutex`, etc.). Stamped on every line
    /// so `scripts/bench_summary.sh` can demux multiple bin outputs.
    pub variant: &'a str,
    /// Client-overlap ratio the bin ran at (0..=100). Stamped so the
    /// summary script can key its tail-latency / RSS table on
    /// `(variant, overlap)`.
    pub overlap_pct: u8,
    /// Transactions drained.
    pub tx_count: usize,
    /// Account count produced.
    pub accounts: usize,
    /// Wall-clock elapsed across the timed region.
    pub elapsed: std::time::Duration,
    /// Peak resident set size in kibibytes from
    /// [`crate::mem::peak_rss_kb`].
    pub peak_rss_kb: i64,
    /// Apply latency percentiles in nanoseconds.
    pub p50_ns: u64,
    /// Apply latency p90 in nanoseconds.
    pub p90_ns: u64,
    /// Apply latency p99 in nanoseconds.
    pub p99_ns: u64,
}

/// Print the line shape that `scripts/bench_summary.sh` parses out of
/// each one-shot bench bin's stdout.
///
/// Format: `variant=NAME overlap=N tx=N accounts=N elapsed_ns=N
/// peak_rss_kb=N p50_ns=N p90_ns=N p99_ns=N`. The script reads each
/// `key=value` token without caring about order; we keep the order
/// stable for human readability.
pub fn print_one_shot_report(report: &OneShotReport<'_>) {
    let OneShotReport {
        variant,
        overlap_pct,
        tx_count,
        accounts,
        elapsed,
        peak_rss_kb,
        p50_ns,
        p90_ns,
        p99_ns,
    } = *report;
    let elapsed_ns = elapsed.as_nanos();
    println!(
        "variant={variant} overlap={overlap_pct} tx={tx_count} accounts={accounts} elapsed_ns={elapsed_ns} peak_rss_kb={peak_rss_kb} p50_ns={p50_ns} p90_ns={p90_ns} p99_ns={p99_ns}",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_should_be_deterministic_for_a_given_seed() {
        let a = generate(WorkloadParams {
            tx_count: 10_000,
            ..Default::default()
        });
        let b = generate(WorkloadParams {
            tx_count: 10_000,
            ..Default::default()
        });

        assert_eq!(a, b);
    }

    #[test]
    fn generate_should_produce_requested_tx_count() {
        let workload = generate(WorkloadParams {
            tx_count: 1234,
            ..Default::default()
        });

        assert_eq!(workload.len(), 1234);
    }

    #[test]
    fn generate_should_emit_deposit_for_first_lifecycle_pick_when_pool_empty() {
        // First-tick fallback: lifecycle events with an empty deposit pool
        // are rewritten to deposits so the very first row is always
        // dispute-friendly. Concretely the workload must contain at least
        // one deposit; without the fallback an unlucky seed could emit a
        // dispute as the first tx and the engine would warn-loop.
        let workload = generate(WorkloadParams {
            tx_count: 1,
            ..Default::default()
        });

        let kind = match workload[0] {
            Transaction::Deposit { .. } => "deposit",
            Transaction::Withdrawal { .. } => "withdrawal",
            _ => "lifecycle",
        };
        assert!(
            kind == "deposit" || kind == "withdrawal",
            "first tx should be a deposit or withdrawal, got {kind}",
        );
    }

    #[test]
    fn generate_should_diverge_under_different_seeds() {
        let a = generate(WorkloadParams {
            tx_count: 1_000,
            seed: 1,
            ..Default::default()
        });
        let b = generate(WorkloadParams {
            tx_count: 1_000,
            seed: 2,
            ..Default::default()
        });

        assert_ne!(a, b);
    }
}
