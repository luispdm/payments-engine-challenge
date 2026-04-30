//! Baseline engine implementation.
//!
//! Per-client account state lives in [`Engine`] alongside a tx ledger that
//! powers dispute lookups. Transaction parsing, errors and CSV glue live in
//! submodules. The full dispute lifecycle (dispute / resolve / chargeback)
//! and post-chargeback lock semantics ship in this version.

use std::collections::HashMap;

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
    /// All five transaction kinds are now wired in. Lock semantics (Q2):
    /// once an account has been frozen by a chargeback, subsequent deposits,
    /// withdrawals, and *new* disputes are rejected. Resolves and chargebacks
    /// targeting txs already in `Disputed` state still process so disputes
    /// opened before the freeze can settle.
    ///
    /// # Errors
    ///
    /// - [`EngineError::InsufficientFunds`] when a withdrawal would drive
    ///   `available` below zero.
    /// - [`EngineError::TxNotFound`] when a dispute / resolve / chargeback
    ///   references an unknown tx id.
    /// - [`EngineError::WithdrawalDispute`] when a dispute references a
    ///   withdrawal (unreachable until task 06 stores withdrawal markers).
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
    /// so logging can attach in task 06.
    pub fn process(&mut self, tx: Transaction) -> Result<(), EngineError> {
        match tx {
            Transaction::Deposit { client, tx, amount } => {
                if Self::account_locked(&self.accounts, client) {
                    return Err(EngineError::AccountLocked { client, tx });
                }
                self.accounts
                    .entry(client)
                    .or_insert_with(|| Account::new(client))
                    .apply_deposit(amount);
                // Task 06 will add duplicate-tx-id detection here; for now
                // valid input guarantees uniqueness.
                self.txs
                    .insert(tx, TxRecord::Deposit(DepositRecord::new(client, amount)));
                Ok(())
            }
            Transaction::Withdrawal { client, tx, amount } => {
                if Self::account_locked(&self.accounts, client) {
                    return Err(EngineError::AccountLocked { client, tx });
                }
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
mod tests {
    use rust_decimal::Decimal;

    use super::*;
    use ledger::DisputeState;

    #[test]
    fn process_should_apply_deposit_to_target_client_account() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "12.3456".parse().unwrap(),
            })
            .unwrap();

        let mut expected = Account::new(1);
        expected.apply_deposit("12.3456".parse().unwrap());
        assert_eq!(engine.accounts.get(&1), Some(&expected));
    }

    #[test]
    fn process_should_auto_create_account_when_client_unseen() {
        let mut engine = Engine::new();

        engine
            .process(Transaction::Deposit {
                client: 9,
                tx: 1,
                amount: "1.0000".parse().unwrap(),
            })
            .unwrap();

        assert!(engine.accounts.contains_key(&9));
    }

    #[test]
    fn process_should_accumulate_when_multiple_deposits_same_client() {
        let mut engine = Engine::new();

        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "1.0000".parse().unwrap(),
            })
            .unwrap();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 2,
                amount: "2.5000".parse().unwrap(),
            })
            .unwrap();

        assert_eq!(
            engine.accounts.get(&1).unwrap().available(),
            "3.5000".parse::<Decimal>().unwrap()
        );
    }

    #[test]
    fn process_should_apply_withdrawal_to_target_client_account() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();

        engine
            .process(Transaction::Withdrawal {
                client: 1,
                tx: 2,
                amount: "4.0000".parse().unwrap(),
            })
            .unwrap();

        assert_eq!(
            engine.accounts.get(&1).unwrap().available(),
            "6.0000".parse::<Decimal>().unwrap()
        );
    }

    #[test]
    fn process_should_return_insufficient_funds_when_withdrawal_exceeds_available() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "1.0000".parse().unwrap(),
            })
            .unwrap();

        let err = engine
            .process(Transaction::Withdrawal {
                client: 1,
                tx: 2,
                amount: "5.0000".parse().unwrap(),
            })
            .unwrap_err();

        assert!(matches!(
            err,
            EngineError::InsufficientFunds {
                client: 1,
                tx: 2,
                ..
            }
        ));
    }

    #[test]
    fn process_should_leave_balances_unchanged_when_withdrawal_rejected() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "1.0000".parse().unwrap(),
            })
            .unwrap();

        engine
            .process(Transaction::Withdrawal {
                client: 1,
                tx: 2,
                amount: "5.0000".parse().unwrap(),
            })
            .unwrap_err();

        assert_eq!(
            engine.accounts.get(&1).unwrap().available(),
            "1.0000".parse::<Decimal>().unwrap()
        );
    }

    #[test]
    fn process_should_auto_create_account_at_zero_when_withdrawal_for_unseen_client() {
        let mut engine = Engine::new();

        let err = engine
            .process(Transaction::Withdrawal {
                client: 7,
                tx: 1,
                amount: "1.0000".parse().unwrap(),
            })
            .unwrap_err();

        assert!(matches!(err, EngineError::InsufficientFunds { .. }));
        let acct = engine.accounts.get(&7).expect("account auto-created");
        assert_eq!(acct.available(), Decimal::ZERO);
    }

    #[test]
    fn process_should_settle_to_correct_balance_for_mixed_deposit_withdrawal_sequence() {
        let mut engine = Engine::new();
        let txs = [
            Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "5.0000".parse().unwrap(),
            },
            Transaction::Withdrawal {
                client: 1,
                tx: 2,
                amount: "1.5000".parse().unwrap(),
            },
            Transaction::Deposit {
                client: 1,
                tx: 3,
                amount: "2.0000".parse().unwrap(),
            },
            Transaction::Withdrawal {
                client: 1,
                tx: 4,
                amount: "0.2500".parse().unwrap(),
            },
        ];

        for tx in txs {
            engine.process(tx).unwrap();
        }

        assert_eq!(
            engine.accounts.get(&1).unwrap().available(),
            "5.2500".parse::<Decimal>().unwrap()
        );
    }

    #[test]
    fn process_should_hold_funds_when_dispute_targets_existing_deposit() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();

        engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap();

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.available(), Decimal::ZERO);
        assert_eq!(acct.held(), "10.0000".parse::<Decimal>().unwrap());
        assert_eq!(acct.total(), "10.0000".parse::<Decimal>().unwrap());
    }

    #[test]
    fn process_should_transition_state_to_disputed_when_dispute_succeeds() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();

        engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap();

        let TxRecord::Deposit(record) = engine.txs.get(&1).unwrap();
        assert_eq!(record.state(), DisputeState::Disputed);
    }

    #[test]
    fn process_should_return_tx_not_found_when_dispute_targets_unknown_tx() {
        let mut engine = Engine::new();

        let err = engine
            .process(Transaction::Dispute { client: 1, tx: 99 })
            .unwrap_err();

        assert!(matches!(err, EngineError::TxNotFound { client: 1, tx: 99 }));
    }

    #[test]
    fn process_should_leave_balances_unchanged_when_dispute_targets_unknown_tx() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "5.0000".parse().unwrap(),
            })
            .unwrap();

        engine
            .process(Transaction::Dispute { client: 1, tx: 99 })
            .unwrap_err();

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.available(), "5.0000".parse::<Decimal>().unwrap());
        assert_eq!(acct.held(), Decimal::ZERO);
    }

    #[test]
    fn process_should_return_already_disputed_when_dispute_fires_twice() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();
        engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap();

        let err = engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap_err();

        assert!(matches!(
            err,
            EngineError::AlreadyDisputed { client: 1, tx: 1 }
        ));
    }

    #[test]
    fn process_should_leave_balances_unchanged_when_dispute_repeats() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();
        engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap();

        let _ = engine.process(Transaction::Dispute { client: 1, tx: 1 });

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.available(), Decimal::ZERO);
        assert_eq!(acct.held(), "10.0000".parse::<Decimal>().unwrap());
    }

    #[test]
    fn process_should_return_client_mismatch_when_dispute_client_differs_from_deposit() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();

        let err = engine
            .process(Transaction::Dispute { client: 2, tx: 1 })
            .unwrap_err();

        assert!(matches!(
            err,
            EngineError::ClientMismatch { client: 2, tx: 1 }
        ));
    }

    #[test]
    fn process_should_leave_balances_unchanged_when_dispute_client_mismatches() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();

        let _ = engine.process(Transaction::Dispute { client: 2, tx: 1 });

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.available(), "10.0000".parse::<Decimal>().unwrap());
        assert_eq!(acct.held(), Decimal::ZERO);
    }

    // TODO(task 06): once `TxRecord::Withdrawal` lands, add a test for
    // dispute-on-withdrawal returning `EngineError::WithdrawalDispute`.
    // Variant exists already so the engine API is stable.

    #[test]
    fn process_should_release_held_funds_when_resolve_targets_disputed_deposit() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();
        engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap();

        engine
            .process(Transaction::Resolve { client: 1, tx: 1 })
            .unwrap();

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.available(), "10.0000".parse::<Decimal>().unwrap());
        assert_eq!(acct.held(), Decimal::ZERO);
        assert_eq!(acct.total(), "10.0000".parse::<Decimal>().unwrap());
    }

    #[test]
    fn process_should_transition_state_to_not_disputed_when_resolve_succeeds() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();
        engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap();

        engine
            .process(Transaction::Resolve { client: 1, tx: 1 })
            .unwrap();

        let TxRecord::Deposit(record) = engine.txs.get(&1).unwrap();
        assert_eq!(record.state(), DisputeState::NotDisputed);
    }

    #[test]
    fn process_should_return_tx_not_found_when_resolve_targets_unknown_tx() {
        let mut engine = Engine::new();

        let err = engine
            .process(Transaction::Resolve { client: 1, tx: 99 })
            .unwrap_err();

        assert!(matches!(err, EngineError::TxNotFound { client: 1, tx: 99 }));
    }

    #[test]
    fn process_should_return_not_disputed_when_resolve_targets_undisputed_tx() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();

        let err = engine
            .process(Transaction::Resolve { client: 1, tx: 1 })
            .unwrap_err();

        assert!(matches!(err, EngineError::NotDisputed { client: 1, tx: 1 }));
    }

    #[test]
    fn process_should_leave_balances_unchanged_when_resolve_targets_undisputed_tx() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();

        let _ = engine.process(Transaction::Resolve { client: 1, tx: 1 });

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.available(), "10.0000".parse::<Decimal>().unwrap());
        assert_eq!(acct.held(), Decimal::ZERO);
    }

    #[test]
    fn process_should_return_client_mismatch_when_resolve_client_differs_from_deposit() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();
        engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap();

        let err = engine
            .process(Transaction::Resolve { client: 2, tx: 1 })
            .unwrap_err();

        assert!(matches!(
            err,
            EngineError::ClientMismatch { client: 2, tx: 1 }
        ));
    }

    #[test]
    fn process_should_leave_balances_unchanged_when_resolve_client_mismatches() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();
        engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap();

        let _ = engine.process(Transaction::Resolve { client: 2, tx: 1 });

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.available(), Decimal::ZERO);
        assert_eq!(acct.held(), "10.0000".parse::<Decimal>().unwrap());
    }

    #[test]
    fn process_should_hold_funds_again_when_dispute_fires_after_resolve() {
        // Per Q5, re-dispute after resolve is allowed: the state machine
        // returns to `NotDisputed` so a second `Dispute` reapplies the hold.
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();
        engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap();
        engine
            .process(Transaction::Resolve { client: 1, tx: 1 })
            .unwrap();

        engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap();

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.available(), Decimal::ZERO);
        assert_eq!(acct.held(), "10.0000".parse::<Decimal>().unwrap());
        assert_eq!(acct.total(), "10.0000".parse::<Decimal>().unwrap());
    }

    /// Drive deposit → dispute on a single tx so the next call exercises the
    /// `Disputed` branch. Used by every chargeback / locked-account test.
    fn deposit_and_dispute(engine: &mut Engine, client: u16, tx: u32, amount: &str) {
        engine
            .process(Transaction::Deposit {
                client,
                tx,
                amount: amount.parse().unwrap(),
            })
            .unwrap();
        engine.process(Transaction::Dispute { client, tx }).unwrap();
    }

    #[test]
    fn process_should_drop_held_when_chargeback_targets_disputed_deposit() {
        let mut engine = Engine::new();
        deposit_and_dispute(&mut engine, 1, 1, "10.0000");

        engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap();

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.available(), Decimal::ZERO);
        assert_eq!(acct.held(), Decimal::ZERO);
        assert_eq!(acct.total(), Decimal::ZERO);
    }

    #[test]
    fn process_should_lock_account_when_chargeback_targets_disputed_deposit() {
        let mut engine = Engine::new();
        deposit_and_dispute(&mut engine, 1, 1, "10.0000");

        engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap();

        assert!(engine.accounts.get(&1).unwrap().locked());
    }

    #[test]
    fn process_should_transition_state_to_charged_back_when_chargeback_succeeds() {
        let mut engine = Engine::new();
        deposit_and_dispute(&mut engine, 1, 1, "10.0000");

        engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap();

        let TxRecord::Deposit(record) = engine.txs.get(&1).unwrap();
        assert_eq!(record.state(), DisputeState::ChargedBack);
    }

    #[test]
    fn process_should_return_tx_not_found_when_chargeback_targets_unknown_tx() {
        let mut engine = Engine::new();

        let err = engine
            .process(Transaction::Chargeback { client: 1, tx: 99 })
            .unwrap_err();

        assert!(matches!(err, EngineError::TxNotFound { client: 1, tx: 99 }));
    }

    #[test]
    fn process_should_return_not_disputed_when_chargeback_targets_undisputed_tx() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "10.0000".parse().unwrap(),
            })
            .unwrap();

        let err = engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap_err();

        assert!(matches!(err, EngineError::NotDisputed { client: 1, tx: 1 }));
    }

    #[test]
    fn process_should_return_not_disputed_when_chargeback_repeats() {
        let mut engine = Engine::new();
        deposit_and_dispute(&mut engine, 1, 1, "10.0000");
        engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap();

        let err = engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap_err();

        assert!(matches!(err, EngineError::NotDisputed { client: 1, tx: 1 }));
    }

    #[test]
    fn process_should_return_client_mismatch_when_chargeback_client_differs_from_deposit() {
        let mut engine = Engine::new();
        deposit_and_dispute(&mut engine, 1, 1, "10.0000");

        let err = engine
            .process(Transaction::Chargeback { client: 2, tx: 1 })
            .unwrap_err();

        assert!(matches!(
            err,
            EngineError::ClientMismatch { client: 2, tx: 1 }
        ));
    }

    #[test]
    fn process_should_return_account_locked_when_deposit_targets_locked_account() {
        let mut engine = Engine::new();
        deposit_and_dispute(&mut engine, 1, 1, "10.0000");
        engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap();

        let err = engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 2,
                amount: "5.0000".parse().unwrap(),
            })
            .unwrap_err();

        assert!(matches!(
            err,
            EngineError::AccountLocked { client: 1, tx: 2 }
        ));
    }

    #[test]
    fn process_should_leave_balances_unchanged_when_deposit_targets_locked_account() {
        let mut engine = Engine::new();
        deposit_and_dispute(&mut engine, 1, 1, "10.0000");
        engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap();

        let _ = engine.process(Transaction::Deposit {
            client: 1,
            tx: 2,
            amount: "5.0000".parse().unwrap(),
        });

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.available(), Decimal::ZERO);
        assert_eq!(acct.held(), Decimal::ZERO);
    }

    #[test]
    fn process_should_return_account_locked_when_withdrawal_targets_locked_account() {
        let mut engine = Engine::new();
        // Set up an account with a positive balance via a separate, untainted
        // deposit so the post-chargeback account still has funds in it that
        // a withdrawal would otherwise clear.
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "5.0000".parse().unwrap(),
            })
            .unwrap();
        deposit_and_dispute(&mut engine, 1, 2, "10.0000");
        engine
            .process(Transaction::Chargeback { client: 1, tx: 2 })
            .unwrap();

        let err = engine
            .process(Transaction::Withdrawal {
                client: 1,
                tx: 3,
                amount: "1.0000".parse().unwrap(),
            })
            .unwrap_err();

        assert!(matches!(
            err,
            EngineError::AccountLocked { client: 1, tx: 3 }
        ));
    }

    #[test]
    fn process_should_leave_balances_unchanged_when_withdrawal_targets_locked_account() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "5.0000".parse().unwrap(),
            })
            .unwrap();
        deposit_and_dispute(&mut engine, 1, 2, "10.0000");
        engine
            .process(Transaction::Chargeback { client: 1, tx: 2 })
            .unwrap();
        let before = engine.accounts.get(&1).unwrap().available();

        let _ = engine.process(Transaction::Withdrawal {
            client: 1,
            tx: 3,
            amount: "1.0000".parse().unwrap(),
        });

        assert_eq!(engine.accounts.get(&1).unwrap().available(), before);
    }

    #[test]
    fn process_should_return_account_locked_when_new_dispute_targets_locked_account() {
        let mut engine = Engine::new();
        // Deposit tx 2 stays undisputed at lock time; the dispute on tx 1
        // locks the account; the dispute on tx 2 is a new dispute and must
        // be rejected per Q2.
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 2,
                amount: "5.0000".parse().unwrap(),
            })
            .unwrap();
        deposit_and_dispute(&mut engine, 1, 1, "10.0000");
        engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap();

        let err = engine
            .process(Transaction::Dispute { client: 1, tx: 2 })
            .unwrap_err();

        assert!(matches!(
            err,
            EngineError::AccountLocked { client: 1, tx: 2 }
        ));
    }

    #[test]
    fn process_should_release_held_when_resolve_targets_disputed_tx_on_locked_account() {
        // Per Q2 a resolve on a tx already in `Disputed` is allowed even on
        // a locked account: the dispute pre-dates the lock.
        let mut engine = Engine::new();
        deposit_and_dispute(&mut engine, 1, 1, "10.0000");
        deposit_and_dispute(&mut engine, 1, 2, "5.0000");
        // Lock the account by charging back tx 1; tx 2 stays in `Disputed`.
        engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap();

        engine
            .process(Transaction::Resolve { client: 1, tx: 2 })
            .unwrap();

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.held(), Decimal::ZERO);
        assert_eq!(acct.available(), "5.0000".parse::<Decimal>().unwrap());
        assert!(acct.locked());
    }

    #[test]
    fn process_should_drop_held_when_chargeback_targets_disputed_tx_on_locked_account() {
        // Per Q2 a chargeback on a tx already in `Disputed` is allowed even
        // on a locked account: settles a pre-lock dispute.
        let mut engine = Engine::new();
        deposit_and_dispute(&mut engine, 1, 1, "10.0000");
        deposit_and_dispute(&mut engine, 1, 2, "5.0000");
        engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap();

        engine
            .process(Transaction::Chargeback { client: 1, tx: 2 })
            .unwrap();

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.held(), Decimal::ZERO);
        assert_eq!(acct.available(), Decimal::ZERO);
        assert_eq!(acct.total(), Decimal::ZERO);
    }

    #[test]
    fn process_should_drive_total_negative_when_fraud_sequence_charges_back() {
        // Fraud sequence per Q3: deposit 100, withdraw 80, dispute,
        // chargeback. End state is `available = -80, held = 0, total = -80,
        // locked = true`.
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "100.0000".parse().unwrap(),
            })
            .unwrap();
        engine
            .process(Transaction::Withdrawal {
                client: 1,
                tx: 2,
                amount: "80.0000".parse().unwrap(),
            })
            .unwrap();
        engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap();

        engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap();

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.available(), "-80.0000".parse::<Decimal>().unwrap());
        assert_eq!(acct.held(), Decimal::ZERO);
        assert_eq!(acct.total(), "-80.0000".parse::<Decimal>().unwrap());
        assert!(acct.locked());
    }

    #[test]
    fn process_should_return_charged_back_when_dispute_targets_charged_back_tx() {
        // Per Q5 a charged-back tx is terminal. A follow-up dispute hits the
        // distinct `ChargedBack` error so logging can tell it apart from a
        // double-dispute on a still-disputed tx.
        let mut engine = Engine::new();
        deposit_and_dispute(&mut engine, 1, 1, "10.0000");
        engine
            .process(Transaction::Chargeback { client: 1, tx: 1 })
            .unwrap();

        let err = engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap_err();

        assert!(matches!(err, EngineError::ChargedBack { client: 1, tx: 1 }));
    }
}
