//! Per-client account state.
//!
//! Balances are stored as [`rust_decimal::Decimal`] for exact 4-decimal
//! arithmetic. The invariant `total == available + held` is enforced by
//! construction: `total` is a derived view computed from `available + held`,
//! never a stored field. All mutation goes through methods on this type so
//! the invariant cannot drift.

use rust_decimal::Decimal;

/// Snapshot of a single client's balances and lock status.
///
/// Fields are private; readers go through accessor methods. This keeps every
/// mutation in one place and avoids callers desyncing the `total` view from
/// `available + held`.
#[derive(Debug, PartialEq, Eq)]
pub struct Account {
    client: u16,
    available: Decimal,
    held: Decimal,
    locked: bool,
}

impl Account {
    /// Create a fresh account with zeroed balances.
    pub fn new(client: u16) -> Self {
        Self {
            client,
            available: Decimal::ZERO,
            held: Decimal::ZERO,
            locked: false,
        }
    }

    /// Client id.
    pub fn client(&self) -> u16 {
        self.client
    }

    /// Funds available for withdrawal.
    pub fn available(&self) -> Decimal {
        self.available
    }

    /// Funds held pending dispute resolution.
    pub fn held(&self) -> Decimal {
        self.held
    }

    /// Derived view: `available + held`.
    pub fn total(&self) -> Decimal {
        self.available + self.held
    }

    /// True after a chargeback has frozen the account.
    pub fn locked(&self) -> bool {
        self.locked
    }

    /// Credit `amount` to `available`. `held` is unchanged, so `total`
    /// (derived) increases by `amount`.
    pub fn apply_deposit(&mut self, amount: Decimal) {
        self.available += amount;
    }

    /// Debit `amount` from `available`. `held` is unchanged, so `total`
    /// (derived) decreases by `amount`.
    ///
    /// # Errors
    ///
    /// Returns [`InsufficientFunds`] when `amount > available`. Balances are
    /// left untouched in that case so the caller can ignore or log per the
    /// spec's silent-rejection rule.
    pub fn apply_withdrawal(&mut self, amount: Decimal) -> Result<(), InsufficientFunds> {
        if amount > self.available {
            return Err(InsufficientFunds);
        }
        self.available -= amount;
        Ok(())
    }

    /// Move `amount` from `available` to `held`. `total` (derived) is
    /// unchanged. Per Q3 a hold may drive `available` negative; that
    /// correctly models post-fraud exposure and preserves the
    /// `total = available + held` invariant.
    pub fn apply_hold(&mut self, amount: Decimal) {
        self.available -= amount;
        self.held += amount;
    }

    /// Inverse of [`Account::apply_hold`]: move `amount` from `held` back to
    /// `available`. `total` (derived) is unchanged. Callers are expected to
    /// pass an `amount` backed by a prior matching hold; passing `amount >
    /// held` violates that contract and drives `held` negative, but the
    /// `total = available + held` invariant still holds.
    pub fn apply_release(&mut self, amount: Decimal) {
        self.held -= amount;
        self.available += amount;
    }

    /// Reverse a held deposit and lock the account.
    ///
    /// Drops `amount` from `held` without crediting `available`, so `total`
    /// (derived) decreases by `amount`. Per Q3 nothing is clamped: if a
    /// withdrawal had already drained the deposited funds, `available` is
    /// already negative and stays negative, correctly modelling post-fraud
    /// exposure.
    pub fn apply_chargeback(&mut self, amount: Decimal) {
        self.held -= amount;
        self.locked = true;
    }
}

/// Returned by [`Account::apply_withdrawal`] when the requested debit would
/// drive `available` below zero. Carries no payload: the engine reattaches
/// the offending `client`/`tx`/`amount` when promoting it to
/// [`super::error::EngineError::InsufficientFunds`].
#[derive(Debug, PartialEq, Eq)]
pub struct InsufficientFunds;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_should_zero_all_balances_for_given_client() {
        assert_eq!(
            Account::new(7),
            Account {
                client: 7,
                available: Decimal::ZERO,
                held: Decimal::ZERO,
                locked: false,
            },
        );
    }

    #[test]
    fn apply_deposit_should_credit_available_and_leave_held() {
        let mut acct = Account::new(1);
        acct.apply_deposit("10.1234".parse().unwrap());

        assert_eq!(
            acct,
            Account {
                client: 1,
                available: "10.1234".parse().unwrap(),
                held: Decimal::ZERO,
                locked: false,
            },
        );
    }

    #[test]
    fn apply_deposit_should_accumulate_without_float_drift() {
        let mut acct = Account::new(1);
        let cent: Decimal = "0.0001".parse().unwrap();

        for _ in 0..10_000 {
            acct.apply_deposit(cent);
        }

        assert_eq!(acct.available(), "1.0000".parse::<Decimal>().unwrap());
    }

    #[test]
    fn apply_withdrawal_should_debit_available_and_leave_held_when_funds_sufficient() {
        let mut acct = Account::new(1);
        acct.apply_deposit("10.0000".parse().unwrap());

        acct.apply_withdrawal("3.5000".parse().unwrap()).unwrap();

        assert_eq!(
            acct,
            Account {
                client: 1,
                available: "6.5000".parse().unwrap(),
                held: Decimal::ZERO,
                locked: false,
            },
        );
    }

    #[test]
    fn apply_withdrawal_should_return_err_when_amount_exceeds_available() {
        let mut acct = Account::new(1);
        acct.apply_deposit("1.0000".parse().unwrap());

        let err = acct
            .apply_withdrawal("1.0001".parse().unwrap())
            .unwrap_err();

        assert_eq!(err, InsufficientFunds);
    }

    #[test]
    fn apply_withdrawal_should_leave_balances_unchanged_when_amount_exceeds_available() {
        let mut acct = Account::new(1);
        acct.apply_deposit("1.0000".parse().unwrap());

        acct.apply_withdrawal("1.0001".parse().unwrap())
            .unwrap_err();

        assert_eq!(
            acct,
            Account {
                client: 1,
                available: "1.0000".parse().unwrap(),
                held: Decimal::ZERO,
                locked: false,
            },
        );
    }

    #[test]
    fn apply_withdrawal_should_succeed_when_amount_equals_available() {
        let mut acct = Account::new(1);
        acct.apply_deposit("2.5000".parse().unwrap());

        acct.apply_withdrawal("2.5000".parse().unwrap()).unwrap();

        assert_eq!(acct.available(), Decimal::ZERO);
    }

    #[test]
    fn apply_hold_should_move_amount_from_available_to_held() {
        let mut acct = Account::new(1);
        acct.apply_deposit("10.0000".parse().unwrap());

        acct.apply_hold("3.0000".parse().unwrap());

        assert_eq!(
            acct,
            Account {
                client: 1,
                available: "7.0000".parse().unwrap(),
                held: "3.0000".parse().unwrap(),
                locked: false,
            },
        );
    }

    #[test]
    fn apply_hold_should_leave_total_unchanged() {
        let mut acct = Account::new(1);
        acct.apply_deposit("10.0000".parse().unwrap());
        let total_before = acct.total();

        acct.apply_hold("3.0000".parse().unwrap());

        assert_eq!(acct.total(), total_before);
    }

    #[test]
    fn apply_hold_should_drive_available_negative_when_amount_exceeds_balance() {
        // Per Q3, holds may take `available` below zero so the
        // `total = available + held` invariant survives post-fraud states.
        let mut acct = Account::new(1);
        acct.apply_deposit("1.0000".parse().unwrap());

        acct.apply_hold("5.0000".parse().unwrap());

        assert_eq!(acct.available(), "-4.0000".parse::<Decimal>().unwrap());
        assert_eq!(acct.held(), "5.0000".parse::<Decimal>().unwrap());
        assert_eq!(acct.total(), "1.0000".parse::<Decimal>().unwrap());
    }

    #[test]
    fn apply_release_should_move_amount_from_held_back_to_available() {
        let mut acct = Account::new(1);
        acct.apply_deposit("10.0000".parse().unwrap());
        acct.apply_hold("3.0000".parse().unwrap());

        acct.apply_release("3.0000".parse().unwrap());

        assert_eq!(
            acct,
            Account {
                client: 1,
                available: "10.0000".parse().unwrap(),
                held: Decimal::ZERO,
                locked: false,
            },
        );
    }

    #[test]
    fn apply_release_should_leave_total_unchanged() {
        let mut acct = Account::new(1);
        acct.apply_deposit("10.0000".parse().unwrap());
        acct.apply_hold("3.0000".parse().unwrap());
        let total_before = acct.total();

        acct.apply_release("3.0000".parse().unwrap());

        assert_eq!(acct.total(), total_before);
    }

    #[test]
    fn apply_chargeback_should_drop_held_without_crediting_available() {
        let mut acct = Account::new(1);
        acct.apply_deposit("10.0000".parse().unwrap());
        acct.apply_hold("3.0000".parse().unwrap());

        acct.apply_chargeback("3.0000".parse().unwrap());

        assert_eq!(
            acct,
            Account {
                client: 1,
                available: "7.0000".parse().unwrap(),
                held: Decimal::ZERO,
                locked: true,
            },
        );
    }

    #[test]
    fn apply_chargeback_should_lock_account() {
        let mut acct = Account::new(1);
        acct.apply_deposit("10.0000".parse().unwrap());
        acct.apply_hold("10.0000".parse().unwrap());

        acct.apply_chargeback("10.0000".parse().unwrap());

        assert!(acct.locked());
    }

    #[test]
    fn apply_chargeback_should_drop_total_by_amount() {
        let mut acct = Account::new(1);
        acct.apply_deposit("10.0000".parse().unwrap());
        acct.apply_hold("3.0000".parse().unwrap());
        let total_before = acct.total();

        acct.apply_chargeback("3.0000".parse().unwrap());

        assert_eq!(
            acct.total(),
            total_before - "3.0000".parse::<Decimal>().unwrap()
        );
    }

    #[test]
    fn apply_chargeback_should_drive_total_negative_for_fraud_sequence() {
        // Fraud sequence per Q3: deposit then withdraw, then chargeback
        // reverses the original credit. `available` was already drained so
        // the chargeback drops it past zero.
        let mut acct = Account::new(1);
        acct.apply_deposit("100.0000".parse().unwrap());
        acct.apply_withdrawal("80.0000".parse().unwrap()).unwrap();
        acct.apply_hold("100.0000".parse().unwrap());

        acct.apply_chargeback("100.0000".parse().unwrap());

        assert_eq!(
            acct,
            Account {
                client: 1,
                available: "-80.0000".parse().unwrap(),
                held: Decimal::ZERO,
                locked: true,
            },
        );
    }
}
