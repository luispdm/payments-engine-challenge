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
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_should_set_client_id() {
        let acct = Account::new(7);

        assert_eq!(acct.client(), 7);
    }

    #[test]
    fn new_should_zero_available() {
        let acct = Account::new(1);

        assert_eq!(acct.available(), Decimal::ZERO);
    }

    #[test]
    fn new_should_zero_held() {
        let acct = Account::new(1);

        assert_eq!(acct.held(), Decimal::ZERO);
    }

    #[test]
    fn new_should_zero_total() {
        let acct = Account::new(1);

        assert_eq!(acct.total(), Decimal::ZERO);
    }

    #[test]
    fn new_should_be_unlocked() {
        let acct = Account::new(1);

        assert!(!acct.locked());
    }

    #[test]
    fn apply_deposit_should_credit_available() {
        let mut acct = Account::new(1);
        let amount: Decimal = "10.1234".parse().unwrap();

        acct.apply_deposit(amount);

        assert_eq!(acct.available(), amount);
    }

    #[test]
    fn apply_deposit_should_increase_total_by_amount() {
        let mut acct = Account::new(1);
        let amount: Decimal = "10.1234".parse().unwrap();

        acct.apply_deposit(amount);

        assert_eq!(acct.total(), amount);
    }

    #[test]
    fn apply_deposit_should_leave_held_unchanged() {
        let mut acct = Account::new(1);

        acct.apply_deposit("5.0000".parse().unwrap());

        assert_eq!(acct.held(), Decimal::ZERO);
    }

    #[test]
    fn total_should_equal_available_plus_held() {
        let acct = Account::new(1);

        assert_eq!(acct.total(), acct.available() + acct.held());
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
}
