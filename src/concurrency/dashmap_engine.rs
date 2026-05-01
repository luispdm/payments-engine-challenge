//! `DashMap`-backed engine. Sharded interior mutability lets disjoint
//! clients (and disjoint tx ids) progress in parallel without blocking
//! each other; same-shard ops serialize on the shard's `RwLock`, which
//! is correct.
//!
//! Lock-order discipline. Whenever the engine holds locks across two
//! maps, the order is **deposits → accounts**. Deposit / withdrawal
//! handlers avoid multi-shard holds altogether and run as a sequence
//! of independent atomic ops on each map; that yields a small TOCTOU
//! window between the `account_locked` read and the subsequent
//! `apply_*` (a concurrent chargeback could lock the account in
//! between), which is the intrinsic cost of dropping the global lock.
//! At 0% client overlap the window is empty (no two threads share a
//! client) and the cross-variant equality gate holds.

use std::io::{Read, Write};
use std::thread;
use std::time::Instant;

use dashmap::{DashMap, DashSet, mapref::entry::Entry};
use rust_decimal::Decimal;

use crate::bench_support::ProducerStream;
use crate::engine::account::Account;
use crate::engine::error::EngineError;
use crate::engine::io::{drive_input, write_output};
use crate::engine::ledger::{DepositRecord, DisputeRejection, DisputeState};
use crate::engine::transaction::Transaction;

use super::workload::{LatencyRecorder, WorkloadResult, sorted_accounts};

/// Sharded engine. Three concurrent maps replace the production
/// engine's three single-threaded ones; the API mirrors the
/// production [`crate::engine::Engine`] but takes `&self` everywhere
/// (interior mutability via [`DashMap`] / [`DashSet`]).
#[derive(Default)]
pub struct DashEngine {
    accounts: DashMap<u16, Account>,
    deposits: DashMap<u32, DepositRecord>,
    seen_txs: DashSet<u32>,
}

impl DashEngine {
    /// Construct an empty engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one transaction. Mirrors [`crate::engine::Engine::process`]
    /// one-for-one in observable behavior.
    ///
    /// # Errors
    /// Forwards every variant of [`EngineError`] from the production
    /// engine's `process`. See that doc-comment for the full catalogue.
    pub fn submit(&self, tx: Transaction) -> Result<(), EngineError> {
        match tx {
            Transaction::Deposit { client, tx, amount } => self.apply_deposit(client, tx, amount),
            Transaction::Withdrawal { client, tx, amount } => {
                self.apply_withdrawal(client, tx, amount)
            }
            Transaction::Dispute { client, tx } => self.apply_dispute(client, tx),
            Transaction::Resolve { client, tx } => self.apply_resolve(client, tx),
            Transaction::Chargeback { client, tx } => self.apply_chargeback(client, tx),
        }
    }

    fn apply_deposit(&self, client: u16, tx: u32, amount: Decimal) -> Result<(), EngineError> {
        if amount <= Decimal::ZERO {
            return Err(EngineError::NonPositiveAmount { client, tx, amount });
        }
        if self.account_locked(client) {
            return Err(EngineError::AccountLocked { client, tx });
        }
        if !self.seen_txs.insert(tx) {
            return Err(EngineError::DuplicateTxId { client, tx });
        }
        self.deposits.insert(tx, DepositRecord::new(client, amount));
        self.accounts
            .entry(client)
            .or_insert_with(|| Account::new(client))
            .apply_deposit(amount);
        Ok(())
    }

    fn apply_withdrawal(&self, client: u16, tx: u32, amount: Decimal) -> Result<(), EngineError> {
        if amount <= Decimal::ZERO {
            return Err(EngineError::NonPositiveAmount { client, tx, amount });
        }
        if self.account_locked(client) {
            return Err(EngineError::AccountLocked { client, tx });
        }
        if !self.seen_txs.insert(tx) {
            return Err(EngineError::DuplicateTxId { client, tx });
        }
        self.accounts
            .entry(client)
            .or_insert_with(|| Account::new(client))
            .apply_withdrawal(amount)
            .map_err(|_| EngineError::InsufficientFunds { client, tx, amount })
    }

    fn apply_dispute(&self, client: u16, tx: u32) -> Result<(), EngineError> {
        // Locking discipline: hold the deposits-shard write guard while
        // reading accounts to keep the state-transition + lock-check
        // pair atomic. Order is deposits → accounts; never the reverse.
        let amount = match self.deposits.entry(tx) {
            Entry::Vacant(_) => {
                return Err(self.classify_missing(client, tx));
            }
            Entry::Occupied(mut occ) => {
                let dep = occ.get_mut();
                if dep.client() != client {
                    return Err(EngineError::ClientMismatch { client, tx });
                }
                if dep.state() == DisputeState::NotDisputed && self.account_locked(client) {
                    return Err(EngineError::AccountLocked { client, tx });
                }
                dep.try_dispute().map_err(|e| match e {
                    DisputeRejection::AlreadyDisputed => {
                        EngineError::AlreadyDisputed { client, tx }
                    }
                    DisputeRejection::ChargedBack => EngineError::ChargedBack { client, tx },
                })?
            }
        };
        self.accounts
            .entry(client)
            .or_insert_with(|| Account::new(client))
            .apply_hold(amount);
        Ok(())
    }

    fn apply_resolve(&self, client: u16, tx: u32) -> Result<(), EngineError> {
        let amount = match self.deposits.entry(tx) {
            Entry::Vacant(_) => return Err(self.classify_missing(client, tx)),
            Entry::Occupied(mut occ) => {
                let dep = occ.get_mut();
                if dep.client() != client {
                    return Err(EngineError::ClientMismatch { client, tx });
                }
                dep.try_resolve()
                    .map_err(|_| EngineError::NotDisputed { client, tx })?
            }
        };
        self.accounts
            .entry(client)
            .or_insert_with(|| Account::new(client))
            .apply_release(amount);
        Ok(())
    }

    fn apply_chargeback(&self, client: u16, tx: u32) -> Result<(), EngineError> {
        let amount = match self.deposits.entry(tx) {
            Entry::Vacant(_) => return Err(self.classify_missing(client, tx)),
            Entry::Occupied(mut occ) => {
                let dep = occ.get_mut();
                if dep.client() != client {
                    return Err(EngineError::ClientMismatch { client, tx });
                }
                dep.try_chargeback()
                    .map_err(|_| EngineError::NotDisputed { client, tx })?
            }
        };
        // Per Q2 a chargeback on a tx already in `Disputed` is permitted
        // even if the account is locked, so no lock check.
        self.accounts
            .entry(client)
            .or_insert_with(|| Account::new(client))
            .apply_chargeback(amount);
        Ok(())
    }

    fn account_locked(&self, client: u16) -> bool {
        self.accounts.get(&client).is_some_and(|a| a.locked())
    }

    /// Classify a deposit-map miss into the right "not-disputable"
    /// engine error: `WithdrawalDispute` if the tx id was registered as
    /// a withdrawal (presence in `seen_txs`), otherwise `TxNotFound`.
    fn classify_missing(&self, client: u16, tx: u32) -> EngineError {
        if self.seen_txs.contains(&tx) {
            EngineError::WithdrawalDispute { client, tx }
        } else {
            EngineError::TxNotFound { client, tx }
        }
    }

    /// Deterministic, client-sorted snapshot of every known account.
    pub fn snapshot(&self) -> Vec<Account> {
        let cloned = self.accounts.iter().map(|r| r.value().clone());
        sorted_accounts(cloned)
    }
}

/// CSV-driven entry point for the test runner.
///
/// # Errors
/// Propagates structural IO failures from the CSV reader / writer.
pub fn run<R: Read, W: Write>(input: R, output: W) -> anyhow::Result<()> {
    let engine = DashEngine::new();
    drive_input(input, |tx| engine.submit(tx))?;
    write_output(output, &engine.snapshot())
}

/// Multi-producer bench driver.
pub fn run_workload(producers: usize, streams: Vec<ProducerStream>) -> WorkloadResult {
    use std::sync::Arc;

    assert_eq!(
        streams.len(),
        producers,
        "stream count must match producer count",
    );
    let engine = Arc::new(DashEngine::new());
    let bench_start = Instant::now();

    let mut join_handles = Vec::with_capacity(producers);
    for stream in streams {
        let engine = Arc::clone(&engine);
        join_handles.push(thread::spawn(move || {
            let mut recorder = LatencyRecorder::new();
            for tx in stream.txs {
                let sent_at_ns = bench_start.elapsed().as_nanos() as u64;
                let _ = engine.submit(tx);
                recorder.record(bench_start, sent_at_ns);
            }
            recorder
        }));
    }

    let mut recorders = Vec::with_capacity(producers);
    for h in join_handles {
        recorders.push(h.join().expect("producer thread panicked"));
    }

    WorkloadResult {
        accounts: engine.snapshot(),
        recorders,
    }
}
