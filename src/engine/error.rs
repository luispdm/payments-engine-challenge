//! Engine error variants.
//!
//! The engine is side effect free: it returns structured errors and the
//! driver loop in `main` decides which to surface and which to swallow.

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

    /// Deposit or withdrawal carries a non-positive amount (`<= 0`).
    /// stays free for a corrected retry.
    #[error("transaction {tx} for client {client}: amount {amount} must be strictly positive")]
    NonPositiveAmount {
        /// Client id.
        client: u16,
        /// Tx id of the rejected row.
        tx: u32,
        /// Offending amount.
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

    /// Dispute / resolve / chargeback event references a withdrawal.
    /// Withdrawals are not disputable.
    #[error("transaction {tx} for client {client}: withdrawals are not disputable")]
    WithdrawalDispute {
        /// Client id from the offending row.
        client: u16,
        /// Referenced tx id.
        tx: u32,
    },

    /// Dispute event fired against a tx that is already in `Disputed`
    /// state. This is a no-op (idempotent).
    #[error("transaction {tx} for client {client}: already disputed")]
    AlreadyDisputed {
        /// Client id from the offending row.
        client: u16,
        /// Referenced tx id.
        tx: u32,
    },

    /// Resolve / chargeback event fired against a tx that is not currently
    /// in `Disputed` state.
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
    /// A charged-back tx accepts no further dispute lifecycle events.
    #[error("transaction {tx} for client {client}: tx already charged back")]
    ChargedBack {
        /// Client id from the offending row.
        client: u16,
        /// Referenced tx id.
        tx: u32,
    },

    /// Deposit, withdrawal, or new dispute targeted an account locked by a
    /// previous chargeback. Only resolves and chargebacks on disputes
    /// opened before the lock continue to process.
    #[error("transaction {tx} for client {client}: account is locked")]
    AccountLocked {
        /// Client id from the offending row.
        client: u16,
        /// Referenced tx id.
        tx: u32,
    },

    /// Deposit or withdrawal row reused a tx id that has already been seen.
    /// The second event is rejected and never touches account state.
    #[error("transaction {tx} for client {client}: duplicate tx id")]
    DuplicateTxId {
        /// Client id from the offending row.
        client: u16,
        /// Referenced tx id.
        tx: u32,
    },
}
