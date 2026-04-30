//! Per-transaction ledger entries.
//!
//! The engine retains a copy of every deposit so later dispute events can
//! recover the original client and amount, cross-check them, and walk the
//! dispute lifecycle. Withdrawals join the ledger as a marker variant in
//! task 06 to enforce cross-type tx-id dedup; the variant is intentionally
//! absent here so a missing arm forces that task to wire it up.

use rust_decimal::Decimal;

/// Lifecycle state of a deposit with respect to disputes.
///
/// `ChargedBack` arrives in task 05; until then the state machine has just
/// the initial state and `Disputed`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DisputeState {
    /// Initial state; hold has not been applied.
    NotDisputed,
    /// `dispute` has fired; funds are held pending resolution.
    Disputed,
}

/// Stored snapshot of a deposit, sufficient to service the dispute
/// lifecycle without re-reading the input.
///
/// Fields are private; mutation goes through the typed transition methods so
/// the state machine cannot be skipped.
#[derive(Debug, PartialEq, Eq)]
pub struct DepositRecord {
    client: u16,
    amount: Decimal,
    state: DisputeState,
}

impl DepositRecord {
    /// Record a fresh deposit. State starts at `NotDisputed`.
    pub fn new(client: u16, amount: Decimal) -> Self {
        Self {
            client,
            amount,
            state: DisputeState::NotDisputed,
        }
    }

    /// Client that originally made the deposit.
    pub fn client(&self) -> u16 {
        self.client
    }

    /// Original deposited amount.
    pub fn amount(&self) -> Decimal {
        self.amount
    }

    /// Current dispute lifecycle state.
    pub fn state(&self) -> DisputeState {
        self.state
    }

    /// Transition `NotDisputed -> Disputed`. Caller must check the state
    /// first; the engine surfaces `AlreadyDisputed` rather than allowing a
    /// silent re-hold.
    pub fn mark_disputed(&mut self) {
        self.state = DisputeState::Disputed;
    }
}

/// Tx ledger entry. Only `Deposit` exists at this stage; the `Withdrawal`
/// marker variant lands in task 06 to power cross-type dedup.
#[derive(Debug, PartialEq, Eq)]
pub enum TxRecord {
    /// Deposit row, retained so disputes can find it later.
    Deposit(DepositRecord),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_should_record_deposit_at_not_disputed_state() {
        let record = DepositRecord::new(1, "10.0000".parse().unwrap());

        assert_eq!(record.state(), DisputeState::NotDisputed);
    }

    #[test]
    fn mark_disputed_should_transition_state_to_disputed() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());

        record.mark_disputed();

        assert_eq!(record.state(), DisputeState::Disputed);
    }
}
