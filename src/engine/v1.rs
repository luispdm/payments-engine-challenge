//! Baseline engine implementation. Subsequent tasks fill in transaction
//! processing, account state, and dispute lifecycle.

pub mod account;
pub mod error;
pub mod io;
pub mod transaction;

/// Holds per-client account state and the global transaction ledger.
pub struct Engine;
