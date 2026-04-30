//! Baseline engine implementation.
//!
//! Per-client account state lives in [`Engine`] alongside a tx ledger that
//! powers dispute lookups. Transaction parsing, errors and CSV glue live in
//! submodules. Subsequent tasks add resolve / chargeback transitions and
//! lock semantics.

use std::collections::HashMap;

pub mod account;
pub mod error;
pub(crate) mod io;
pub mod ledger;
pub mod transaction;

use account::Account;
use error::EngineError;
use ledger::{DepositRecord, DisputeState, TxRecord};
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
    /// Tasks 01-03 act on [`Transaction::Deposit`], [`Transaction::Withdrawal`]
    /// and [`Transaction::Dispute`]; resolve and chargeback parse cleanly but
    /// no-op until later tasks wire them up.
    ///
    /// # Errors
    ///
    /// - [`EngineError::InsufficientFunds`] when a withdrawal would drive
    ///   `available` below zero.
    /// - [`EngineError::TxNotFound`] when a dispute references an unknown
    ///   tx id.
    /// - [`EngineError::WithdrawalDispute`] when a dispute references a
    ///   withdrawal (unreachable until task 06 stores withdrawal markers).
    /// - [`EngineError::AlreadyDisputed`] when a dispute fires against a tx
    ///   already in `Disputed` state (idempotent re-dispute, per Q5).
    /// - [`EngineError::ClientMismatch`] when a dispute's client_id does not
    ///   match the recorded deposit's client.
    ///
    /// Per spec the driver swallows all of the above; variants are returned
    /// so logging can attach in task 06.
    pub fn process(&mut self, tx: Transaction) -> Result<(), EngineError> {
        match tx {
            Transaction::Deposit { client, tx, amount } => {
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
            Transaction::Withdrawal { client, tx, amount } => self
                .accounts
                .entry(client)
                .or_insert_with(|| Account::new(client))
                .apply_withdrawal(amount)
                .map_err(|_| EngineError::InsufficientFunds { client, tx, amount }),
            Transaction::Dispute { client, tx } => self.apply_dispute(client, tx),
            Transaction::Resolve { .. } | Transaction::Chargeback { .. } => Ok(()),
        }
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
                if deposit.state() == DisputeState::Disputed {
                    return Err(EngineError::AlreadyDisputed { client, tx });
                }
                let amount = deposit.amount();
                deposit.mark_disputed();
                // The deposit was processed earlier, which auto-creates the
                // account. No code path removes accounts, so the lookup
                // cannot fail.
                self.accounts
                    .get_mut(&client)
                    .expect("account exists since deposit was processed")
                    .apply_hold(amount);
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
}
