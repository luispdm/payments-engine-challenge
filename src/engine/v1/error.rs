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

    /// Dispute / resolve / chargeback referenced a tx that the engine has
    /// never seen.
    #[error("transaction {tx} for client {client}: not found in tx ledger")]
    TxNotFound {
        /// Client id from the offending row.
        client: u16,
        /// Referenced tx id.
        tx: u32,
    },

    /// Dispute event references a withdrawal; per Q1 only deposits are
    /// disputable. Unreachable until task 06 stores withdrawal markers.
    #[error("transaction {tx} for client {client}: withdrawals are not disputable")]
    WithdrawalDispute {
        /// Client id from the offending row.
        client: u16,
        /// Referenced tx id.
        tx: u32,
    },

    /// Dispute event fired against a tx that is already in `Disputed`
    /// state. Per Q5 this is a no-op (idempotent).
    #[error("transaction {tx} for client {client}: already disputed")]
    AlreadyDisputed {
        /// Client id from the offending row.
        client: u16,
        /// Referenced tx id.
        tx: u32,
    },

    /// Resolve / chargeback event fired against a tx that is not currently
    /// in `Disputed` state. Per spec the row is a partner error and ignored.
    #[error("transaction {tx} for client {client}: not currently disputed")]
    NotDisputed {
        /// Client id from the offending row.
        client: u16,
        /// Referenced tx id.
        tx: u32,
    },

    /// Dispute / resolve / chargeback event references a tx whose stored
    /// client_id differs from the row's client_id. Treated as a partner error.
    #[error("transaction {tx}: client {client} does not match the recorded client")]
    ClientMismatch {
        /// Client id from the offending row.
        client: u16,
        /// Referenced tx id.
        tx: u32,
    },

    /// Dispute event fired against a tx in the terminal `ChargedBack` state.
    /// Per Q5 a charged-back tx accepts no further dispute lifecycle events.
    #[error("transaction {tx} for client {client}: tx already charged back")]
    ChargedBack {
        /// Client id from the offending row.
        client: u16,
        /// Referenced tx id.
        tx: u32,
    },

    /// Deposit, withdrawal, or new dispute targeted an account locked by a
    /// previous chargeback. Per Q2 only resolves and chargebacks on disputes
    /// opened before the lock continue to process.
    #[error("transaction {tx} for client {client}: account is locked")]
    AccountLocked {
        /// Client id from the offending row.
        client: u16,
        /// Referenced tx id.
        tx: u32,
    },
}
