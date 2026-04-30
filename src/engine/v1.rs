//! Baseline engine implementation.
//!
//! Per-client account state lives in [`Engine`]; transaction parsing,
//! errors and CSV glue live in submodules. Subsequent tasks add the
//! transaction ledger, dispute lifecycle and lock semantics.

use std::collections::HashMap;

pub mod account;
pub mod error;
pub mod io;
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
    /// Task 01 only acts on [`Transaction::Deposit`]; the other variants
    /// parse cleanly but no-op until later tasks wire them up.
    ///
    /// # Errors
    ///
    /// Currently infallible; returns a `Result` so the signature is stable
    /// as later tasks add error variants (insufficient funds, unknown tx,
    /// account locked, …).
    pub fn process(&mut self, tx: Transaction) -> Result<(), EngineError> {
        match tx {
            Transaction::Deposit { client, amount, .. } => {
                self.accounts
                    .entry(client)
                    .or_insert_with(|| Account::new(client))
                    .apply_deposit(amount);
                Ok(())
            }
            Transaction::Withdrawal { .. }
            | Transaction::Dispute { .. }
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
    fn process_should_credit_available_and_total_when_deposit() {
        let mut engine = Engine::new();
        let amount: Decimal = "12.3456".parse().unwrap();

        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount,
            })
            .unwrap();

        let acct = engine.accounts.get(&1).unwrap();
        assert_eq!(acct.available, amount);
        assert_eq!(acct.total, amount);
        assert_eq!(acct.held, Decimal::ZERO);
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
            engine.accounts.get(&1).unwrap().available,
            "3.5000".parse::<Decimal>().unwrap()
        );
    }

    #[test]
    fn process_should_noop_when_withdrawal() {
        let mut engine = Engine::new();

        engine
            .process(Transaction::Withdrawal {
                client: 1,
                tx: 1,
                amount: "1.0000".parse().unwrap(),
            })
            .unwrap();

        assert!(engine.accounts.is_empty());
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
