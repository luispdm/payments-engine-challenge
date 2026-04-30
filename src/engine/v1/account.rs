//! Per-client account state.
//!
//! Balances are stored as [`rust_decimal::Decimal`] for exact 4-decimal
//! arithmetic. The invariant `total == available + held` must hold after
//! every state transition; mutations go through methods on this type so
//! the invariant is enforced in one place.

use rust_decimal::Decimal;

/// Snapshot of a single client's balances and lock status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Client id.
    pub client: u16,
    /// Funds available for withdrawal.
    pub available: Decimal,
    /// Funds held pending dispute resolution.
    pub held: Decimal,
    /// `available + held`. Stored to avoid recomputing on every output row.
    pub total: Decimal,
    /// True after a chargeback has frozen the account.
    pub locked: bool,
}

impl Account {
    /// Create a fresh account with zeroed balances.
    pub fn new(client: u16) -> Self {
        Self {
            client,
            available: Decimal::ZERO,
            held: Decimal::ZERO,
            total: Decimal::ZERO,
            locked: false,
        }
    }

    /// Credit `amount` to `available` and `total`. `held` is unchanged.
    pub fn apply_deposit(&mut self, amount: Decimal) {
        self.available += amount;
        self.total += amount;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_should_zero_all_balances() {
        let acct = Account::new(7);

        assert_eq!(acct.client, 7);
        assert_eq!(acct.available, Decimal::ZERO);
        assert_eq!(acct.held, Decimal::ZERO);
        assert_eq!(acct.total, Decimal::ZERO);
        assert!(!acct.locked);
    }

    #[test]
    fn apply_deposit_should_credit_available_and_total() {
        let mut acct = Account::new(1);
        let amount: Decimal = "10.1234".parse().unwrap();

        acct.apply_deposit(amount);

        assert_eq!(acct.available, amount);
        assert_eq!(acct.total, amount);
        assert_eq!(acct.held, Decimal::ZERO);
    }

    #[test]
    fn apply_deposit_should_leave_held_unchanged() {
        let mut acct = Account::new(1);

        acct.apply_deposit("5.0000".parse().unwrap());

        assert_eq!(acct.held, Decimal::ZERO);
    }

    #[test]
    fn apply_deposit_should_accumulate_without_float_drift() {
        let mut acct = Account::new(1);
        let cent: Decimal = "0.0001".parse().unwrap();

        for _ in 0..10_000 {
            acct.apply_deposit(cent);
        }

        assert_eq!(acct.available, "1.0000".parse::<Decimal>().unwrap());
        assert_eq!(acct.total, "1.0000".parse::<Decimal>().unwrap());
    }
}
