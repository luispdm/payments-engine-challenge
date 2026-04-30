//! Transaction event types.
//!
//! [`Transaction`] is the engine's input vocabulary. The CSV pipeline
//! deserializes each row into a [`RawTransaction`] and converts it into a
//! [`Transaction`], surfacing partner errors (unknown type, missing amount)
//! as [`EngineError`] variants the driver loop can ignore per spec.

use rust_decimal::Decimal;
use serde::Deserialize;

use super::error::EngineError;

/// Strongly typed transaction handed to [`super::Engine::process`].
///
/// Withdrawal, dispute, resolve and chargeback variants are recognised at
/// task 01 so the parser never crashes on a well-formed CSV; the engine
/// only acts on `Deposit` until later tasks wire up the rest.
///
/// `Clone` is derived so benchmark drivers can replay a pre-generated
/// workload across multiple criterion iterations without re-running the
/// generator inside the timed region.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Transaction {
    /// Credit `amount` to client's `available` and `total`.
    Deposit {
        /// Client id.
        client: u16,
        /// Globally unique tx id.
        tx: u32,
        /// Funds being credited (4 decimal places per spec).
        amount: Decimal,
    },
    /// Debit `amount` from client's `available` and `total`.
    Withdrawal {
        /// Client id.
        client: u16,
        /// Globally unique tx id.
        tx: u32,
        /// Funds being debited.
        amount: Decimal,
    },
    /// Open a dispute against an earlier deposit. No-op until task 03.
    Dispute {
        /// Client id.
        client: u16,
        /// Tx id of the disputed transaction.
        tx: u32,
    },
    /// Close a dispute, releasing held funds. No-op until task 04.
    Resolve {
        /// Client id.
        client: u16,
        /// Tx id of the resolved dispute.
        tx: u32,
    },
    /// Reverse a disputed deposit and lock the account. No-op until task 05.
    Chargeback {
        /// Client id.
        client: u16,
        /// Tx id of the charged-back transaction.
        tx: u32,
    },
}

/// CSV-shaped row before validation. The csv crate deserializes directly
/// into this; conversion to [`Transaction`] enforces invariants.
#[derive(Debug, Deserialize)]
pub struct RawTransaction {
    /// Transaction kind: `deposit`, `withdrawal`, `dispute`, `resolve`, `chargeback`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Client id.
    pub client: u16,
    /// Globally unique tx id.
    pub tx: u32,
    /// Optional amount; required for deposit / withdrawal, absent otherwise.
    pub amount: Option<Decimal>,
}

impl TryFrom<RawTransaction> for Transaction {
    type Error = EngineError;

    fn try_from(raw: RawTransaction) -> Result<Self, Self::Error> {
        let RawTransaction {
            kind,
            client,
            tx,
            amount,
        } = raw;

        // Match on the lowercased kind so `Deposit`, `WITHDRAWAL`, etc. do not
        // silently get dropped as `UnknownTransactionType` by the driver loop.
        match kind.to_ascii_lowercase().as_str() {
            "deposit" => {
                let amount = amount.ok_or(EngineError::MissingAmount { tx })?;
                Ok(Transaction::Deposit { client, tx, amount })
            }
            "withdrawal" => {
                let amount = amount.ok_or(EngineError::MissingAmount { tx })?;
                Ok(Transaction::Withdrawal { client, tx, amount })
            }
            "dispute" => Ok(Transaction::Dispute { client, tx }),
            "resolve" => Ok(Transaction::Resolve { client, tx }),
            "chargeback" => Ok(Transaction::Chargeback { client, tx }),
            _ => Err(EngineError::UnknownTransactionType { kind }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kind: &str, amount: Option<&str>) -> RawTransaction {
        RawTransaction {
            kind: kind.to_string(),
            client: 1,
            tx: 42,
            amount: amount.map(|a| a.parse().unwrap()),
        }
    }

    #[test]
    fn try_from_should_build_deposit_when_kind_is_deposit() {
        let tx = Transaction::try_from(raw("deposit", Some("10.5000"))).unwrap();

        assert_eq!(
            tx,
            Transaction::Deposit {
                client: 1,
                tx: 42,
                amount: "10.5000".parse().unwrap(),
            }
        );
    }

    #[test]
    fn try_from_should_build_withdrawal_when_kind_is_withdrawal() {
        let tx = Transaction::try_from(raw("withdrawal", Some("3.0000"))).unwrap();

        assert!(matches!(tx, Transaction::Withdrawal { .. }));
    }

    #[test]
    fn try_from_should_build_dispute_when_kind_is_dispute() {
        let tx = Transaction::try_from(raw("dispute", None)).unwrap();

        assert_eq!(tx, Transaction::Dispute { client: 1, tx: 42 });
    }

    #[test]
    fn try_from_should_build_resolve_when_kind_is_resolve() {
        let tx = Transaction::try_from(raw("resolve", None)).unwrap();

        assert_eq!(tx, Transaction::Resolve { client: 1, tx: 42 });
    }

    #[test]
    fn try_from_should_build_chargeback_when_kind_is_chargeback() {
        let tx = Transaction::try_from(raw("chargeback", None)).unwrap();

        assert_eq!(tx, Transaction::Chargeback { client: 1, tx: 42 });
    }

    #[test]
    fn try_from_should_return_missing_amount_when_deposit_has_no_amount() {
        let err = Transaction::try_from(raw("deposit", None)).unwrap_err();

        assert!(matches!(err, EngineError::MissingAmount { tx: 42 }));
    }

    #[test]
    fn try_from_should_return_missing_amount_when_withdrawal_has_no_amount() {
        let err = Transaction::try_from(raw("withdrawal", None)).unwrap_err();

        assert!(matches!(err, EngineError::MissingAmount { tx: 42 }));
    }

    #[test]
    fn try_from_should_return_unknown_type_when_kind_is_unknown() {
        let err = Transaction::try_from(raw("transfer", None)).unwrap_err();

        assert!(matches!(err, EngineError::UnknownTransactionType { .. }));
    }

    #[test]
    fn try_from_should_accept_uppercase_kind() {
        let tx = Transaction::try_from(raw("DEPOSIT", Some("1.0000"))).unwrap();

        assert!(matches!(tx, Transaction::Deposit { .. }));
    }

    #[test]
    fn try_from_should_accept_mixed_case_kind() {
        let tx = Transaction::try_from(raw("ChargeBack", None)).unwrap();

        assert_eq!(tx, Transaction::Chargeback { client: 1, tx: 42 });
    }
}
