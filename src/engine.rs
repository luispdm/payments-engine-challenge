//! In-memory payments engine.
//!
//! Per-client account state lives in [`Engine`] alongside a split tx ledger
//! that powers dispute lookup and cross-type tx-id dedup.
//! Disputable deposits are kept in `HashMap<u32, DepositRecord>`.
//! 
//! Every tx id (deposit and withdrawal alike) is tracked in `HashSet<u32>`
//! so withdrawal-vs-deposit collisions surface as `DuplicateTxId`.
//! 
//! Transaction parsing, errors and CSV glue live in submodules.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};

use rust_decimal::Decimal;

pub mod account;
pub mod error;
pub(crate) mod io;
pub mod ledger;
pub mod transaction;

use account::Account;
use error::EngineError;
use ledger::{DepositRecord, DisputeRejection, DisputeState};
use transaction::Transaction;

/// Split-ledger payments engine.
///
/// `seen_txs` carries every tx id seen (deposit and withdrawal).
/// `deposits` carries the records the dispute paths walk; a hit in
/// `seen_txs` without a matching entry in `deposits` is by construction a
/// withdrawal id, which is not disputable.
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
    /// Lock semantics: once an account has been frozen by a chargeback,
    /// subsequent deposits, withdrawals, and *new* disputes are rejected.
    /// Resolves and chargebacks targeting txs already in `Disputed` state
    /// still process so disputes opened before the freeze can settle.
    /// The engine also dedups every deposit / withdrawal tx id across types:
    /// a second event reusing an existing id is rejected without touching account state.
    ///
    /// # Errors
    ///
    /// - [`EngineError::NonPositiveAmount`] when a deposit or withdrawal
    ///   carries an amount `<= 0`. Tx id is *not* consumed: the row is
    ///   structurally malformed rather than a recordable attempt.
    /// - [`EngineError::InsufficientFunds`] when a withdrawal would drive
    ///   `available` below zero.
    /// - [`EngineError::DuplicateTxId`] when a deposit or withdrawal reuses
    ///   a tx id already present in the ledger.
    /// - [`EngineError::TxNotFound`] when a dispute / resolve / chargeback
    ///   references an unknown tx id.
    /// - [`EngineError::WithdrawalDispute`] when a dispute / resolve /
    ///   chargeback references a withdrawal.
    /// - [`EngineError::AlreadyDisputed`] when a dispute fires against a tx
    ///   already in `Disputed` state (idempotent re-dispute).
    /// - [`EngineError::ChargedBack`] when a dispute fires against a tx in
    ///   the terminal `ChargedBack` state (the tx is settled).
    /// - [`EngineError::NotDisputed`] when a resolve or chargeback fires
    ///   against a tx that is not currently in `Disputed` state. Includes
    ///   already-charged-back txs.
    /// - [`EngineError::ClientMismatch`] when a dispute / resolve /
    ///   chargeback's client_id does not match the recorded deposit's client.
    /// - [`EngineError::AccountLocked`] when a deposit, withdrawal, or new
    ///   dispute targets an account locked by a prior chargeback.
    ///
    /// Per spec the driver swallows all of the above; variants are returned
    /// so the driver loop can log them to `log::warn!`.
    pub fn process(&mut self, tx: Transaction) -> Result<(), EngineError> {
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

    fn apply_deposit(&mut self, client: u16, tx: u32, amount: Decimal) -> Result<(), EngineError> {
        if amount <= Decimal::ZERO {
            return Err(EngineError::NonPositiveAmount { client, tx, amount });
        }
        if Self::account_locked(&self.accounts, client) {
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

    fn apply_withdrawal(
        &mut self,
        client: u16,
        tx: u32,
        amount: Decimal,
    ) -> Result<(), EngineError> {
        if amount <= Decimal::ZERO {
            return Err(EngineError::NonPositiveAmount { client, tx, amount });
        }
        if Self::account_locked(&self.accounts, client) {
            return Err(EngineError::AccountLocked { client, tx });
        }
        // Reserve the tx id before attempting the debit: an insufficient-funds
        // rejection still consumes the id (this was a genuine attempt by the
        // caller)
        if !self.seen_txs.insert(tx) {
            return Err(EngineError::DuplicateTxId { client, tx });
        }
        self.accounts
            .entry(client)
            .or_insert_with(|| Account::new(client))
            .apply_withdrawal(amount)
            .map_err(|_| EngineError::InsufficientFunds { client, tx, amount })
    }

    /// True when the `client`'s account has received a chargeback.
    /// 
    /// Takes the `accounts` map by reference (rather than `&self`)
    /// so `fn`s holding `&mut self.deposits` can split-borrow
    /// without conflicting on the whole `Engine`.
    fn account_locked(accounts: &HashMap<u16, Account>, client: u16) -> bool {
        accounts.get(&client).is_some_and(Account::locked)
    }

    /// Resolve a dispute-lifecycle event's target tx id to a deposit
    /// record. Returns the appropriate "not-disputable" error if the id is
    /// unknown or references a withdrawal.
    fn get_deposit<'a>(
        deposits: &'a mut HashMap<u32, DepositRecord>,
        seen_txs: &HashSet<u32>,
        client: u16,
        tx: u32,
    ) -> Result<&'a mut DepositRecord, EngineError> {
        if let Entry::Occupied(slot) = deposits.entry(tx) {
            return Ok(slot.into_mut());
        }
        // At this point the tx either does not exist at all or
        // it is referencing a withdrawal (only tracked in `seen_txs`).
        if seen_txs.contains(&tx) {
            Err(EngineError::WithdrawalDispute { client, tx })
        } else {
            Err(EngineError::TxNotFound { client, tx })
        }
    }

    fn apply_dispute(&mut self, client: u16, tx: u32) -> Result<(), EngineError> {
        let deposit = Self::get_deposit(&mut self.deposits, &self.seen_txs, client, tx)?;
        if deposit.client() != client {
            return Err(EngineError::ClientMismatch { client, tx });
        }
        // New disputes are blocked if the account is locked.
        if deposit.state() == DisputeState::NotDisputed
            && Self::account_locked(&self.accounts, client)
        {
            return Err(EngineError::AccountLocked { client, tx });
        }
        let amount = deposit.try_dispute().map_err(|e| match e {
            DisputeRejection::AlreadyDisputed => EngineError::AlreadyDisputed { client, tx },
            DisputeRejection::ChargedBack => EngineError::ChargedBack { client, tx },
        })?;
        // The account already exists here, but we are not using `unwrap`
        // to avoid panicking in case the code changes in the future
        self.accounts
            .entry(client)
            .or_insert_with(|| Account::new(client))
            .apply_hold(amount);
        Ok(())
    }

    fn apply_resolve(&mut self, client: u16, tx: u32) -> Result<(), EngineError> {
        let deposit = Self::get_deposit(&mut self.deposits, &self.seen_txs, client, tx)?;
        if deposit.client() != client {
            return Err(EngineError::ClientMismatch { client, tx });
        }
        let amount = deposit
            .try_resolve()
            .map_err(|_| EngineError::NotDisputed { client, tx })?;
        // The account already exists here, but we are not using `unwrap`
        // to avoid panicking in case the code changes in the future
        self.accounts
            .entry(client)
            .or_insert_with(|| Account::new(client))
            .apply_release(amount);
        Ok(())
    }

    fn apply_chargeback(&mut self, client: u16, tx: u32) -> Result<(), EngineError> {
        let deposit = Self::get_deposit(&mut self.deposits, &self.seen_txs, client, tx)?;
        if deposit.client() != client {
            return Err(EngineError::ClientMismatch { client, tx });
        }
        let amount = deposit
            .try_chargeback()
            .map_err(|_| EngineError::NotDisputed { client, tx })?;
        // A chargeback on a tx already in `Disputed` is permitted
        // even if the account is locked, so no lock check here.
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
