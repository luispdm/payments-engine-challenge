//! Alternate engine implementation under storage option A.
//!
//! Where v1 keeps a single `HashMap<u32, TxRecord>` (option B), v2 splits
//! the ledger into two structures: a `HashMap<u32, DepositRecord>` for
//! disputable deposits and a `HashSet<u32>` for cross-type id dedup. The
//! observable contract matches v1 exactly so the same input feeds both
//! variants in the data-structure benchmark (task 07a). All shared types
//! (`Account`, `Transaction`, `EngineError`, `DepositRecord`,
//! `DisputeState`, …) are re-used from v1; only the storage layout differs.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use rust_decimal::Decimal;

pub mod io;

use super::v1::account::Account;
use super::v1::error::EngineError;
use super::v1::ledger::{DepositRecord, DisputeRejection, DisputeState};
use super::v1::transaction::Transaction;

/// Option-A payments engine.
///
/// The dedup set carries every tx id seen (deposit and withdrawal alike per
/// 6a). The deposit map carries the records v1's dispute paths walk; a hit
/// in `seen_txs` without a matching entry in `deposits` is by construction
/// a withdrawal id, which is not disputable per Q1.
#[derive(Debug, Default)]
pub struct Engine {
    accounts: HashMap<u16, Account>,
    deposits: HashMap<u32, DepositRecord>,
    seen_txs: HashSet<u32>,
}

impl Engine {
    /// Create an empty engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply `tx` to the engine state.
    ///
    /// Same observable behavior as [`super::v1::Engine::process`]. Refer to
    /// that doc comment for the per-variant error semantics; v2 differs
    /// only in storage layout and dispute lookup path.
    ///
    /// # Errors
    ///
    /// See [`super::v1::Engine::process`] — the variants returned by v2
    /// are a strict subset of v1's and carry identical semantics, so the
    /// driver loop swallows the same set of partner errors.
    pub fn process(&mut self, tx: Transaction) -> Result<(), EngineError> {
        match tx {
            Transaction::Deposit { client, tx, amount } => {
                if amount <= Decimal::ZERO {
                    return Err(EngineError::NonPositiveAmount { client, tx, amount });
                }
                if Self::account_locked(&self.accounts, client) {
                    return Err(EngineError::AccountLocked { client, tx });
                }
                if !self.seen_txs.insert(tx) {
                    return Err(EngineError::DuplicateTxId { client, tx });
                }
                // Record the deposit before mutating the account so the
                // ordering matches v1; deposits cannot fail at the account
                // layer so the rollback path is unreachable, but keeping
                // the same shape across variants makes diffing them easy.
                self.deposits.insert(tx, DepositRecord::new(client, amount));
                self.accounts
                    .entry(client)
                    .or_insert_with(|| Account::new(client))
                    .apply_deposit(amount);
                Ok(())
            }
            Transaction::Withdrawal { client, tx, amount } => {
                if amount <= Decimal::ZERO {
                    return Err(EngineError::NonPositiveAmount { client, tx, amount });
                }
                if Self::account_locked(&self.accounts, client) {
                    return Err(EngineError::AccountLocked { client, tx });
                }
                if !self.seen_txs.insert(tx) {
                    return Err(EngineError::DuplicateTxId { client, tx });
                }
                // Reserve the tx id (in `seen_txs` only — withdrawals are
                // not disputable per Q1, so they never enter `deposits`)
                // before attempting the debit; an insufficient-funds
                // rejection still consumes the id, matching v1's behavior
                // and the "globally unique tx ids" rule from 6a.
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

    fn account_locked(accounts: &HashMap<u16, Account>, client: u16) -> bool {
        accounts.get(&client).is_some_and(Account::locked)
    }

    /// Resolve a dispute-lifecycle event's target tx id to a deposit
    /// record. Returns `None` paired with the appropriate
    /// "not-disputable" error if the id is unknown or names a withdrawal.
    fn deposit_mut<'a>(
        deposits: &'a mut HashMap<u32, DepositRecord>,
        seen_txs: &HashSet<u32>,
        client: u16,
        tx: u32,
    ) -> Result<&'a mut DepositRecord, EngineError> {
        if let Entry::Occupied(slot) = deposits.entry(tx) {
            return Ok(slot.into_mut());
        }
        // Falling through to here means the deposit map has no entry; the
        // tx either does not exist at all or names a withdrawal (only
        // tracked in `seen_txs` per 6a). The branch order mirrors v1's
        // match arms so the error variants line up exactly.
        if seen_txs.contains(&tx) {
            Err(EngineError::WithdrawalDispute { client, tx })
        } else {
            Err(EngineError::TxNotFound { client, tx })
        }
    }

    fn apply_dispute(&mut self, client: u16, tx: u32) -> Result<(), EngineError> {
        let deposit = Self::deposit_mut(&mut self.deposits, &self.seen_txs, client, tx)?;
        if deposit.client() != client {
            return Err(EngineError::ClientMismatch { client, tx });
        }
        if deposit.state() == DisputeState::NotDisputed
            && Self::account_locked(&self.accounts, client)
        {
            return Err(EngineError::AccountLocked { client, tx });
        }
        let amount = deposit.try_dispute().map_err(|e| match e {
            DisputeRejection::AlreadyDisputed => EngineError::AlreadyDisputed { client, tx },
            DisputeRejection::ChargedBack => EngineError::ChargedBack { client, tx },
        })?;
        // Defensive `entry` for symmetry with v1: a dispute may target a
        // pre-locked or pre-resolved deposit, but the account row always
        // exists by the time we get here.
        self.accounts
            .entry(client)
            .or_insert_with(|| Account::new(client))
            .apply_hold(amount);
        Ok(())
    }

    fn apply_resolve(&mut self, client: u16, tx: u32) -> Result<(), EngineError> {
        let deposit = Self::deposit_mut(&mut self.deposits, &self.seen_txs, client, tx)?;
        if deposit.client() != client {
            return Err(EngineError::ClientMismatch { client, tx });
        }
        let amount = deposit
            .try_resolve()
            .map_err(|_| EngineError::NotDisputed { client, tx })?;
        self.accounts
            .entry(client)
            .or_insert_with(|| Account::new(client))
            .apply_release(amount);
        Ok(())
    }

    fn apply_chargeback(&mut self, client: u16, tx: u32) -> Result<(), EngineError> {
        let deposit = Self::deposit_mut(&mut self.deposits, &self.seen_txs, client, tx)?;
        if deposit.client() != client {
            return Err(EngineError::ClientMismatch { client, tx });
        }
        let amount = deposit
            .try_chargeback()
            .map_err(|_| EngineError::NotDisputed { client, tx })?;
        self.accounts
            .entry(client)
            .or_insert_with(|| Account::new(client))
            .apply_chargeback(amount);
        Ok(())
    }

    /// Iterate over all known accounts.
    pub fn accounts(&self) -> impl Iterator<Item = &Account> {
        self.accounts.values()
    }
}

#[cfg(test)]
mod tests;
