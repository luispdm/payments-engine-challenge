//! Baseline engine implementation.
//!
//! Per-client account state lives in [`Engine`]; transaction parsing,
//! errors and CSV glue live in submodules. Subsequent tasks add the
//! transaction ledger, dispute lifecycle and lock semantics.

use std::collections::HashMap;

pub mod account;
pub mod error;
pub(crate) mod io;
pub mod transaction;

use account::Account;
use error::EngineError;
use transaction::Transaction;

/// In-memory payments engine. State is the per-client account map; later
/// tasks add a global tx ledger for dedup and dispute lookups.
#[derive(Debug, Default)]
pub struct Engine {
    accounts: HashMap<u16, Account>,
}

impl Engine {
    /// Create an empty engine.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply `tx` to the engine state.
    ///
    /// Tasks 01-02 act on [`Transaction::Deposit`] and
    /// [`Transaction::Withdrawal`]; the dispute lifecycle variants parse
    /// cleanly but no-op until later tasks wire them up.
    ///
    /// # Errors
    ///
    /// - [`EngineError::InsufficientFunds`] when a withdrawal would drive
    ///   `available` below zero. Per spec the driver swallows this; the
    ///   variant is surfaced so logging can attach later.
    pub fn process(&mut self, tx: Transaction) -> Result<(), EngineError> {
        match tx {
            Transaction::Deposit { client, amount, .. } => {
                self.accounts
                    .entry(client)
                    .or_insert_with(|| Account::new(client))
                    .apply_deposit(amount);
                Ok(())
            }
            Transaction::Withdrawal { client, tx, amount } => self
                .accounts
                .entry(client)
                .or_insert_with(|| Account::new(client))
                .apply_withdrawal(amount)
                .map_err(|_| EngineError::InsufficientFunds { client, tx, amount }),
            Transaction::Dispute { .. }
            | Transaction::Resolve { .. }
            | Transaction::Chargeback { .. } => Ok(()),
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
    fn process_should_noop_when_dispute() {
        let mut engine = Engine::new();

        engine
            .process(Transaction::Dispute { client: 1, tx: 1 })
            .unwrap();

        assert!(engine.accounts.is_empty());
    }
}
