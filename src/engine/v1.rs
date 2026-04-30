//! Baseline engine implementation.
//!
//! Per-client account state lives in [`Engine`] alongside a tx ledger that
//! powers dispute lookups. Transaction parsing, errors and CSV glue live in
//! submodules. The full dispute lifecycle (dispute / resolve / chargeback)
//! and post-chargeback lock semantics ship in this version.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

pub mod account;
pub mod error;
pub(crate) mod io;
pub mod ledger;
pub mod transaction;

use account::Account;
use error::EngineError;
use ledger::{DepositRecord, DisputeRejection, DisputeState, TxRecord};
use transaction::Transaction;

/// In-memory payments engine.
///
/// Holds a per-client account map and a per-tx ledger keyed by tx id. The
/// ledger is the sole source of truth for both dispute lookup and (from
/// task 06) cross-type tx-id dedup.
#[derive(Debug, Default)]
pub struct Engine {
    accounts: HashMap<u16, Account>,
    txs: HashMap<u32, TxRecord>,
}

impl Engine {
    /// Create an empty engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply `tx` to the engine state.
    ///
    /// All five transaction kinds are wired in. Lock semantics (Q2): once an
    /// account has been frozen by a chargeback, subsequent deposits,
    /// withdrawals, and *new* disputes are rejected. Resolves and chargebacks
    /// targeting txs already in `Disputed` state still process so disputes
    /// opened before the freeze can settle. Per 6a the engine also dedups
    /// every deposit / withdrawal tx id across types: a second event reusing
    /// an existing id is rejected without touching account state.
    ///
    /// # Errors
    ///
    /// - [`EngineError::InsufficientFunds`] when a withdrawal would drive
    ///   `available` below zero.
    /// - [`EngineError::DuplicateTxId`] when a deposit or withdrawal reuses
    ///   a tx id already present in the ledger (deposit/deposit,
    ///   withdrawal/withdrawal, or cross-type collision).
    /// - [`EngineError::TxNotFound`] when a dispute / resolve / chargeback
    ///   references an unknown tx id.
    /// - [`EngineError::WithdrawalDispute`] when a dispute / resolve /
    ///   chargeback references a withdrawal (per Q1 not disputable).
    /// - [`EngineError::AlreadyDisputed`] when a dispute fires against a tx
    ///   already in `Disputed` state (idempotent re-dispute, per Q5).
    /// - [`EngineError::ChargedBack`] when a dispute fires against a tx in
    ///   the terminal `ChargedBack` state (per Q5 the tx is settled).
    /// - [`EngineError::NotDisputed`] when a resolve or chargeback fires
    ///   against a tx that is not currently in `Disputed` state. Includes
    ///   already-charged-back txs.
    /// - [`EngineError::ClientMismatch`] when a dispute / resolve /
    ///   chargeback's client_id does not match the recorded deposit's client.
    /// - [`EngineError::AccountLocked`] when a deposit, withdrawal, or new
    ///   dispute targets an account locked by a prior chargeback.
    ///
    /// Per spec the driver swallows all of the above; variants are returned
    /// so the driver loop can downgrade them to `log::warn!`.
    pub fn process(&mut self, tx: Transaction) -> Result<(), EngineError> {
        match tx {
            Transaction::Deposit { client, tx, amount } => {
                if Self::account_locked(&self.accounts, client) {
                    return Err(EngineError::AccountLocked { client, tx });
                }
                let Entry::Vacant(slot) = self.txs.entry(tx) else {
                    return Err(EngineError::DuplicateTxId { client, tx });
                };
                slot.insert(TxRecord::Deposit(DepositRecord::new(client, amount)));
                self.accounts
                    .entry(client)
                    .or_insert_with(|| Account::new(client))
                    .apply_deposit(amount);
                Ok(())
            }
            Transaction::Withdrawal { client, tx, amount } => {
                if Self::account_locked(&self.accounts, client) {
                    return Err(EngineError::AccountLocked { client, tx });
                }
                let Entry::Vacant(slot) = self.txs.entry(tx) else {
                    return Err(EngineError::DuplicateTxId { client, tx });
                };
                // Reserve the tx id before attempting the debit so that an
                // insufficient-funds rejection still consumes the id; per
                // spec tx ids are globally unique, and a partner-error retry
                // with the same id should be flagged as a duplicate rather
                // than silently re-attempted.
                slot.insert(TxRecord::Withdrawal);
                self.accounts
                    .entry(client)
                    .or_insert_with(|| Account::new(client))
                    .apply_withdrawal(amount)
                    .map_err(|_| EngineError::InsufficientFunds { client, tx, amount })
            }
            Transaction::Dispute { client, tx } => self.apply_dispute(client, tx),
            Transaction::Resolve { client, tx } => self.apply_resolve(client, tx),
            Transaction::Chargeback { client, tx } => self.apply_chargeback(client, tx),
        }
    }

    /// True when `client` has an account that a prior chargeback locked.
    /// Unseen clients are unlocked by definition. Takes the `accounts` map by
    /// reference (rather than `&self`) so call sites holding `&mut self.txs`
    /// can split-borrow without conflicting on the whole `Engine`.
    fn account_locked(accounts: &HashMap<u16, Account>, client: u16) -> bool {
        accounts.get(&client).is_some_and(Account::locked)
    }

    fn apply_dispute(&mut self, client: u16, tx: u32) -> Result<(), EngineError> {
        let Some(record) = self.txs.get_mut(&tx) else {
            return Err(EngineError::TxNotFound { client, tx });
        };
        match record {
            TxRecord::Withdrawal => Err(EngineError::WithdrawalDispute { client, tx }),
            TxRecord::Deposit(deposit) => {
                if deposit.client() != client {
                    return Err(EngineError::ClientMismatch { client, tx });
                }
                // Per Q2 only *new* disputes (state == NotDisputed) are
                // blocked once the account is locked. Disputed and
                // ChargedBack states fall through to their own state-level
                // errors below, which the driver loop logs the same way.
                if deposit.state() == DisputeState::NotDisputed
                    && Self::account_locked(&self.accounts, client)
                {
                    return Err(EngineError::AccountLocked { client, tx });
                }
                let amount = deposit.try_dispute().map_err(|e| match e {
                    DisputeRejection::AlreadyDisputed => {
                        EngineError::AlreadyDisputed { client, tx }
                    }
                    DisputeRejection::ChargedBack => EngineError::ChargedBack { client, tx },
                })?;
                // Per Q3 a hold may drive `available` negative, so a hold on
                // a freshly-created zero account is well-defined; using
                // `entry` keeps the engine sound even if a future change
                // breaks the deposit-creates-account invariant.
                self.accounts
                    .entry(client)
                    .or_insert_with(|| Account::new(client))
                    .apply_hold(amount);
                Ok(())
            }
        }
    }

    fn apply_resolve(&mut self, client: u16, tx: u32) -> Result<(), EngineError> {
        let Some(record) = self.txs.get_mut(&tx) else {
            return Err(EngineError::TxNotFound { client, tx });
        };
        match record {
            TxRecord::Withdrawal => Err(EngineError::WithdrawalDispute { client, tx }),
            TxRecord::Deposit(deposit) => {
                if deposit.client() != client {
                    return Err(EngineError::ClientMismatch { client, tx });
                }
                let amount = deposit
                    .try_resolve()
                    .map_err(|_| EngineError::NotDisputed { client, tx })?;
                // Mirrors the `apply_dispute` defensive `entry` pattern: by
                // the time a resolve fires the dispute already created the
                // account, but using `entry` keeps the engine sound if that
                // invariant ever changes (eviction, sharding).
                self.accounts
                    .entry(client)
                    .or_insert_with(|| Account::new(client))
                    .apply_release(amount);
                Ok(())
            }
        }
    }

    fn apply_chargeback(&mut self, client: u16, tx: u32) -> Result<(), EngineError> {
        let Some(record) = self.txs.get_mut(&tx) else {
            return Err(EngineError::TxNotFound { client, tx });
        };
        match record {
            TxRecord::Withdrawal => Err(EngineError::WithdrawalDispute { client, tx }),
            TxRecord::Deposit(deposit) => {
                if deposit.client() != client {
                    return Err(EngineError::ClientMismatch { client, tx });
                }
                let amount = deposit
                    .try_chargeback()
                    .map_err(|_| EngineError::NotDisputed { client, tx })?;
                // Per Q2 a chargeback on a tx already in `Disputed` is
                // permitted even if the account is locked, so no lock check
                // here. Same `entry` defence as the other handlers.
                self.accounts
                    .entry(client)
                    .or_insert_with(|| Account::new(client))
                    .apply_chargeback(amount);
                Ok(())
            }
        }
    }

    /// Iterate over all known accounts.
    pub fn accounts(&self) -> impl Iterator<Item = &Account> {
        self.accounts.values()
    }
}

#[cfg(test)]
mod tests;
