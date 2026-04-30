//! Engine error variants.
//!
//! The engine is side effect free: it returns structured errors and the
//! driver loop in `main` decides which to surface and which to swallow per
//! the spec's "ignore on partner error" rule. Variants are added incrementally
//! as later tasks introduce more failure modes.

use rust_decimal::Decimal;

/// Errors produced while parsing input rows or processing transactions.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Row's `type` column does not match any known transaction kind.
    #[error("unknown transaction type: {kind}")]
    UnknownTransactionType {
        /// Raw value of the `type` column.
        kind: String,
    },

    /// Deposit or withdrawal row arrived without an `amount` column.
    #[error("transaction {tx} is missing the amount column")]
    MissingAmount {
        /// Tx id of the offending row.
        tx: u32,
    },

    /// Withdrawal would drive `available` below zero. Balances unchanged.
    #[error("transaction {tx} for client {client}: insufficient funds for withdrawal of {amount}")]
    InsufficientFunds {
        /// Client id.
        client: u16,
        /// Tx id of the rejected withdrawal.
        tx: u32,
        /// Amount the client tried to withdraw.
        amount: Decimal,
    },
}
