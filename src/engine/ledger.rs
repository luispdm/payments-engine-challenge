//! Per-transaction ledger entries.
//!
//! The engine retains a copy of every deposit so later dispute events can
//! recover the original client and amount, cross-check them, and walk the
//! dispute lifecycle. Withdrawals are tracked solely in the engine's
//! `seen_txs` set: they are not disputable, so no fields beyond the
//! tx id itself are needed; presence in the set powers cross-type tx-id
//! dedup (6a).

use rust_decimal::Decimal;

/// Lifecycle state of a deposit with respect to disputes.
///
/// `ChargedBack` is terminal: per Q5 a charged-back tx accepts no further
/// dispute events.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DisputeState {
    /// Initial state; hold has not been applied.
    NotDisputed,
    /// `dispute` has fired; funds are held pending resolution.
    Disputed,
    /// `chargeback` has fired; deposit is permanently reversed and the
    /// owning account is locked.
    ChargedBack,
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

    /// Transition `NotDisputed -> Disputed` and return the held amount.
    ///
    /// Owning the check + setter pair on the record keeps the rule in one
    /// place; `try_resolve` and `try_chargeback` follow the same shape.
    ///
    /// # Errors
    ///
    /// - [`DisputeRejection::AlreadyDisputed`] when the record is currently
    ///   in `Disputed` state. Per Q5 the redundant dispute is a no-op.
    /// - [`DisputeRejection::ChargedBack`] when the record is in the terminal
    ///   `ChargedBack` state. Per Q5 a charged-back tx cannot be re-disputed.
    ///
    /// State is left untouched in either error case.
    pub fn try_dispute(&mut self) -> Result<Decimal, DisputeRejection> {
        match self.state {
            DisputeState::NotDisputed => {
                self.state = DisputeState::Disputed;
                Ok(self.amount)
            }
            DisputeState::Disputed => Err(DisputeRejection::AlreadyDisputed),
            DisputeState::ChargedBack => Err(DisputeRejection::ChargedBack),
        }
    }

    /// Transition `Disputed -> NotDisputed` and return the released amount.
    ///
    /// Per Q5 a resolved record is behaviorally identical to one that was
    /// never disputed, so the state machine drops back to `NotDisputed` and
    /// a future dispute on the same tx is allowed.
    ///
    /// # Errors
    ///
    /// Returns [`NotDisputed`] when the record is not currently in
    /// `Disputed` state. State is left untouched in that case.
    pub fn try_resolve(&mut self) -> Result<Decimal, NotDisputed> {
        if self.state != DisputeState::Disputed {
            return Err(NotDisputed);
        }
        self.state = DisputeState::NotDisputed;
        Ok(self.amount)
    }

    /// Transition `Disputed -> ChargedBack` and return the reversed amount.
    ///
    /// Terminal transition: per Q5 a charged-back record never moves again,
    /// so a follow-up `dispute`, `resolve`, or `chargeback` on the same tx
    /// is always a partner error.
    ///
    /// # Errors
    ///
    /// Returns [`NotDisputed`] when the record is not currently in
    /// `Disputed` state, including the already-charged-back case. State is
    /// left untouched in that case.
    pub fn try_chargeback(&mut self) -> Result<Decimal, NotDisputed> {
        if self.state != DisputeState::Disputed {
            return Err(NotDisputed);
        }
        self.state = DisputeState::ChargedBack;
        Ok(self.amount)
    }
}

/// Returned by [`DepositRecord::try_dispute`] when the record's current state
/// rules out a new dispute. The engine maps each variant to a distinct
/// [`super::error::EngineError`] so logging can distinguish "double dispute"
/// from "dispute on terminal tx".
#[derive(Debug, PartialEq, Eq)]
pub enum DisputeRejection {
    /// Record is already in `Disputed` state.
    AlreadyDisputed,
    /// Record is in the terminal `ChargedBack` state.
    ChargedBack,
}

/// Returned by [`DepositRecord::try_resolve`] and
/// [`DepositRecord::try_chargeback`] when the record is not in `Disputed`
/// state. Carries no payload: the engine reattaches the offending
/// `client`/`tx` when promoting it to
/// [`super::error::EngineError::NotDisputed`].
#[derive(Debug, PartialEq, Eq)]
pub struct NotDisputed;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_should_record_deposit_at_not_disputed_state() {
        let record = DepositRecord::new(1, "10.0000".parse().unwrap());

        assert_eq!(record.state(), DisputeState::NotDisputed);
    }

    #[test]
    fn try_dispute_should_transition_state_to_disputed_and_return_amount() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());

        let amount = record.try_dispute().unwrap();

        assert_eq!(amount, "10.0000".parse::<Decimal>().unwrap());
        assert_eq!(record.state(), DisputeState::Disputed);
    }

    #[test]
    fn try_dispute_should_return_already_disputed_when_record_already_disputed() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());
        record.try_dispute().unwrap();

        let err = record.try_dispute().unwrap_err();

        assert_eq!(err, DisputeRejection::AlreadyDisputed);
    }

    #[test]
    fn try_dispute_should_leave_state_disputed_when_record_already_disputed() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());
        record.try_dispute().unwrap();

        let _ = record.try_dispute();

        assert_eq!(record.state(), DisputeState::Disputed);
    }

    #[test]
    fn try_dispute_should_return_charged_back_when_record_is_charged_back() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());
        record.try_dispute().unwrap();
        record.try_chargeback().unwrap();

        let err = record.try_dispute().unwrap_err();

        assert_eq!(err, DisputeRejection::ChargedBack);
    }

    #[test]
    fn try_dispute_should_leave_state_charged_back_when_record_is_charged_back() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());
        record.try_dispute().unwrap();
        record.try_chargeback().unwrap();

        let _ = record.try_dispute();

        assert_eq!(record.state(), DisputeState::ChargedBack);
    }

    #[test]
    fn try_resolve_should_transition_state_to_not_disputed_and_return_amount() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());
        record.try_dispute().unwrap();

        let amount = record.try_resolve().unwrap();

        assert_eq!(amount, "10.0000".parse::<Decimal>().unwrap());
        assert_eq!(record.state(), DisputeState::NotDisputed);
    }

    #[test]
    fn try_resolve_should_return_not_disputed_when_record_not_in_disputed_state() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());

        let err = record.try_resolve().unwrap_err();

        assert_eq!(err, NotDisputed);
    }

    #[test]
    fn try_resolve_should_leave_state_unchanged_when_record_not_in_disputed_state() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());

        let _ = record.try_resolve();

        assert_eq!(record.state(), DisputeState::NotDisputed);
    }

    #[test]
    fn try_dispute_should_be_allowed_after_resolve() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());
        record.try_dispute().unwrap();
        record.try_resolve().unwrap();

        let amount = record.try_dispute().unwrap();

        assert_eq!(amount, "10.0000".parse::<Decimal>().unwrap());
        assert_eq!(record.state(), DisputeState::Disputed);
    }

    #[test]
    fn try_chargeback_should_transition_state_to_charged_back_and_return_amount() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());
        record.try_dispute().unwrap();

        let amount = record.try_chargeback().unwrap();

        assert_eq!(amount, "10.0000".parse::<Decimal>().unwrap());
        assert_eq!(record.state(), DisputeState::ChargedBack);
    }

    #[test]
    fn try_chargeback_should_return_not_disputed_when_record_not_in_disputed_state() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());

        let err = record.try_chargeback().unwrap_err();

        assert_eq!(err, NotDisputed);
    }

    #[test]
    fn try_chargeback_should_leave_state_unchanged_when_record_not_in_disputed_state() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());

        let _ = record.try_chargeback();

        assert_eq!(record.state(), DisputeState::NotDisputed);
    }

    #[test]
    fn try_chargeback_should_return_not_disputed_when_record_already_charged_back() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());
        record.try_dispute().unwrap();
        record.try_chargeback().unwrap();

        let err = record.try_chargeback().unwrap_err();

        assert_eq!(err, NotDisputed);
    }

    #[test]
    fn try_chargeback_should_leave_state_charged_back_when_record_already_charged_back() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());
        record.try_dispute().unwrap();
        record.try_chargeback().unwrap();

        let _ = record.try_chargeback();

        assert_eq!(record.state(), DisputeState::ChargedBack);
    }

    #[test]
    fn try_resolve_should_return_not_disputed_when_record_already_charged_back() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());
        record.try_dispute().unwrap();
        record.try_chargeback().unwrap();

        let err = record.try_resolve().unwrap_err();

        assert_eq!(err, NotDisputed);
    }

    #[test]
    fn try_resolve_should_leave_state_charged_back_when_record_already_charged_back() {
        let mut record = DepositRecord::new(1, "10.0000".parse().unwrap());
        record.try_dispute().unwrap();
        record.try_chargeback().unwrap();

        let _ = record.try_resolve();

        assert_eq!(record.state(), DisputeState::ChargedBack);
    }
}
