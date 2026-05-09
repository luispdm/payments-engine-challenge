//! Transaction event types.
//!
//! The CSV pipeline deserializes each row into a [`RawTransaction`]
//! and converts it into a [`Transaction`]. Unknown kinds fail at the
//! deserialize boundary; [`EngineError::MissingAmount`] is the only
//! row-level error this module raises.

use std::fmt;

use rust_decimal::Decimal;
use serde::{
    Deserialize,
    de::{self, Deserializer, Visitor},
};

use super::error::EngineError;

/// Strongly typed `type` column. Parsed once at the deserialize boundary
/// with case-insensitive matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionKind {
    Deposit,
    Withdrawal,
    Dispute,
    Resolve,
    Chargeback,
}

const KIND_VARIANTS: &[&str] = &["deposit", "withdrawal", "dispute", "resolve", "chargeback"];

impl<'de> Deserialize<'de> for TransactionKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct KindVisitor;

        impl<'de> Visitor<'de> for KindVisitor {
            type Value = TransactionKind;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("one of: deposit, withdrawal, dispute, resolve, chargeback")
            }

            fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
                match s {
                    s if s.eq_ignore_ascii_case("deposit") => Ok(TransactionKind::Deposit),
                    s if s.eq_ignore_ascii_case("withdrawal") => Ok(TransactionKind::Withdrawal),
                    s if s.eq_ignore_ascii_case("dispute") => Ok(TransactionKind::Dispute),
                    s if s.eq_ignore_ascii_case("resolve") => Ok(TransactionKind::Resolve),
                    s if s.eq_ignore_ascii_case("chargeback") => Ok(TransactionKind::Chargeback),
                    other => Err(E::unknown_variant(other, KIND_VARIANTS)),
                }
            }
        }

        d.deserialize_str(KindVisitor)

        // ----------- alternative implementation -----------
        // let s = <&str>::deserialize(d)?;
        // match s {
        //     s if s.eq_ignore_ascii_case("deposit")    => Ok(Self::Deposit),
        //     s if s.eq_ignore_ascii_case("withdrawal") => Ok(Self::Withdrawal),
        //     s if s.eq_ignore_ascii_case("dispute")    => Ok(Self::Dispute),
        //     s if s.eq_ignore_ascii_case("resolve")    => Ok(Self::Resolve),
        //     s if s.eq_ignore_ascii_case("chargeback") => Ok(Self::Chargeback),
        //     other => Err(de::Error::unknown_variant(other, KIND_VARIANTS)),
        // }
    }
}

/// Strongly typed transaction handed to [`super::Engine::process`].
#[derive(Debug, PartialEq, Eq)]
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
    /// Open a dispute against an earlier deposit.
    Dispute {
        /// Client id.
        client: u16,
        /// Tx id of the disputed transaction.
        tx: u32,
    },
    /// Close a dispute, releasing held funds.
    Resolve {
        /// Client id.
        client: u16,
        /// Tx id of the resolved dispute.
        tx: u32,
    },
    /// Reverse a disputed deposit and lock the account.
    Chargeback {
        /// Client id.
        client: u16,
        /// Tx id of the charged-back transaction.
        tx: u32,
    },
}

/// CSV row before validation. The csv crate deserializes directly
/// into this. Conversion to [`Transaction`] enforces invariants.
#[derive(Debug, Deserialize)]
pub struct RawTransaction {
    #[serde(rename = "type")]
    pub kind: TransactionKind,
    /// Client id.
    pub client: u16,
    /// Globally unique tx id.
    pub tx: u32,
    /// Optional amount required for deposit / withdrawal, absent otherwise.
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

        match kind {
            TransactionKind::Deposit => {
                let amount = amount.ok_or(EngineError::MissingAmount { tx })?;
                Ok(Transaction::Deposit { client, tx, amount })
            }
            TransactionKind::Withdrawal => {
                let amount = amount.ok_or(EngineError::MissingAmount { tx })?;
                Ok(Transaction::Withdrawal { client, tx, amount })
            }
            TransactionKind::Dispute => Ok(Transaction::Dispute { client, tx }),
            TransactionKind::Resolve => Ok(Transaction::Resolve { client, tx }),
            TransactionKind::Chargeback => Ok(Transaction::Chargeback { client, tx }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kind: TransactionKind, amount: Option<&str>) -> RawTransaction {
        RawTransaction {
            kind,
            client: 1,
            tx: 42,
            amount: amount.map(|a| a.parse().unwrap()),
        }
    }

    #[test]
    fn try_from_should_build_deposit_when_kind_is_deposit() {
        let tx = Transaction::try_from(raw(TransactionKind::Deposit, Some("10.5000"))).unwrap();

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
        let tx = Transaction::try_from(raw(TransactionKind::Withdrawal, Some("3.0000"))).unwrap();

        assert!(matches!(tx, Transaction::Withdrawal { .. }));
    }

    #[test]
    fn try_from_should_build_dispute_when_kind_is_dispute() {
        let tx = Transaction::try_from(raw(TransactionKind::Dispute, None)).unwrap();

        assert_eq!(tx, Transaction::Dispute { client: 1, tx: 42 });
    }

    #[test]
    fn try_from_should_build_resolve_when_kind_is_resolve() {
        let tx = Transaction::try_from(raw(TransactionKind::Resolve, None)).unwrap();

        assert_eq!(tx, Transaction::Resolve { client: 1, tx: 42 });
    }

    #[test]
    fn try_from_should_build_chargeback_when_kind_is_chargeback() {
        let tx = Transaction::try_from(raw(TransactionKind::Chargeback, None)).unwrap();

        assert_eq!(tx, Transaction::Chargeback { client: 1, tx: 42 });
    }

    #[test]
    fn try_from_should_return_missing_amount_when_deposit_has_no_amount() {
        let err = Transaction::try_from(raw(TransactionKind::Deposit, None)).unwrap_err();

        assert!(matches!(err, EngineError::MissingAmount { tx: 42 }));
    }

    #[test]
    fn try_from_should_return_missing_amount_when_withdrawal_has_no_amount() {
        let err = Transaction::try_from(raw(TransactionKind::Withdrawal, None)).unwrap_err();

        assert!(matches!(err, EngineError::MissingAmount { tx: 42 }));
    }

    #[derive(Deserialize)]
    struct Holder {
        kind: TransactionKind,
    }

    fn parse_kind(s: &str) -> Result<TransactionKind, csv::Error> {
        let csv = format!("kind\n{s}\n");
        let mut rdr = csv::Reader::from_reader(csv.as_bytes());
        rdr.deserialize::<Holder>().next().unwrap().map(|h| h.kind)
    }

    #[test]
    fn deserialize_should_accept_lowercase_kind() {
        assert_eq!(parse_kind("deposit").unwrap(), TransactionKind::Deposit);
    }

    #[test]
    fn deserialize_should_accept_uppercase_kind() {
        assert_eq!(parse_kind("DEPOSIT").unwrap(), TransactionKind::Deposit);
    }

    #[test]
    fn deserialize_should_accept_mixed_case_kind() {
        assert_eq!(
            parse_kind("ChargeBack").unwrap(),
            TransactionKind::Chargeback
        );
    }

    #[test]
    fn deserialize_should_reject_unknown_kind() {
        assert!(parse_kind("transfer").is_err());
    }
}
