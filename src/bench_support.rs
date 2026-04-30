//! Synthetic workload generator for the engine benchmarks (task 07a, reused
//! by 07b). Output is deterministic for a given `(seed, clients, tx_count)`
//! triple so successive runs of the same harness are directly comparable
//! and the cross-engine correctness gate has a stable oracle.
//!
//! Gated behind the `bench` feature: production builds carry no `rand`
//! dependency. The module is a leaf — it pulls only from `engine::v1` for
//! the [`Transaction`] type so v2 can consume the same `Vec<Transaction>`
//! without introducing a back-edge to `bench_support`.

use rand::{RngExt, SeedableRng, rngs::SmallRng};
use rust_decimal::Decimal;

use crate::engine::v1::transaction::Transaction;

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
