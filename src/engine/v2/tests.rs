//! Unit tests for v2's [`super::Engine::process`].
//!
//! v2 differs from v1 only in storage layout (separate map + set). These
//! tests cover the same behaviors as v1's suite but go through the public
//! API (`accounts()` iterator + error variants) since v2's private fields
//! differ from v1's. The cross-check integration test in
//! `tests/scenarios.rs` complements this suite by asserting v2 matches v1
//! byte-for-byte over the existing CSV scenarios.

use rust_decimal::Decimal;

use super::*;

/// Convenience: look up a single client's account snapshot via the public
/// `accounts()` iterator. Panics if the client has no account, which
/// indicates a test setup bug, not a v2 behavior to assert against.
fn account_for(engine: &Engine, client: u16) -> &Account {
    engine
        .accounts()
        .find(|a| a.client() == client)
        .unwrap_or_else(|| panic!("account for client {client} not found"))
}

/// Drive deposit + dispute on a single tx so the next call exercises the
/// `Disputed` branch. Used by every chargeback / locked-account test.
fn deposit_and_dispute(engine: &mut Engine, client: u16, tx: u32, amount: &str) {
    engine
        .process(Transaction::Deposit {
            client,
            tx,
            amount: amount.parse().unwrap(),
        })
        .unwrap();
    engine.process(Transaction::Dispute { client, tx }).unwrap();
}

#[test]
fn process_should_apply_deposit_to_target_client_account() {
    let mut engine = Engine::new();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "12.3456".parse().unwrap(),
        })
        .unwrap();

    assert_eq!(
        account_for(&engine, 1).available(),
        "12.3456".parse::<Decimal>().unwrap()
    );
}

#[test]
fn process_should_accumulate_when_multiple_deposits_same_client() {
    let mut engine = Engine::new();

    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "1.0000".parse().unwrap(),
        })
        .unwrap();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 2,
            amount: "2.5000".parse().unwrap(),
        })
        .unwrap();

    assert_eq!(
        account_for(&engine, 1).available(),
        "3.5000".parse::<Decimal>().unwrap()
    );
}

#[test]
fn process_should_apply_withdrawal_to_target_client_account() {
    let mut engine = Engine::new();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "10.0000".parse().unwrap(),
        })
        .unwrap();

    engine
        .process(Transaction::Withdrawal {
            client: 1,
            tx: 2,
            amount: "4.0000".parse().unwrap(),
        })
        .unwrap();

    assert_eq!(
        account_for(&engine, 1).available(),
        "6.0000".parse::<Decimal>().unwrap()
    );
}

#[test]
fn process_should_return_insufficient_funds_when_withdrawal_exceeds_available() {
    let mut engine = Engine::new();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "1.0000".parse().unwrap(),
        })
        .unwrap();

    let err = engine
        .process(Transaction::Withdrawal {
            client: 1,
            tx: 2,
            amount: "5.0000".parse().unwrap(),
        })
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::InsufficientFunds {
            client: 1,
            tx: 2,
            ..
        }
    ));
}

#[test]
fn process_should_hold_funds_when_dispute_targets_existing_deposit() {
    let mut engine = Engine::new();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "10.0000".parse().unwrap(),
        })
        .unwrap();

    engine
        .process(Transaction::Dispute { client: 1, tx: 1 })
        .unwrap();

    let acct = account_for(&engine, 1);
    assert_eq!(acct.available(), Decimal::ZERO);
    assert_eq!(acct.held(), "10.0000".parse::<Decimal>().unwrap());
    assert_eq!(acct.total(), "10.0000".parse::<Decimal>().unwrap());
}

#[test]
fn process_should_return_tx_not_found_when_dispute_targets_unknown_tx() {
    let mut engine = Engine::new();

    let err = engine
        .process(Transaction::Dispute { client: 1, tx: 99 })
        .unwrap_err();

    assert!(matches!(err, EngineError::TxNotFound { client: 1, tx: 99 }));
}

#[test]
fn process_should_return_already_disputed_when_dispute_fires_twice() {
    let mut engine = Engine::new();
    deposit_and_dispute(&mut engine, 1, 1, "10.0000");

    let err = engine
        .process(Transaction::Dispute { client: 1, tx: 1 })
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::AlreadyDisputed { client: 1, tx: 1 }
    ));
}

#[test]
fn process_should_return_client_mismatch_when_dispute_client_differs_from_deposit() {
    let mut engine = Engine::new();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "10.0000".parse().unwrap(),
        })
        .unwrap();

    let err = engine
        .process(Transaction::Dispute { client: 2, tx: 1 })
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::ClientMismatch { client: 2, tx: 1 }
    ));
}

#[test]
fn process_should_release_held_funds_when_resolve_targets_disputed_deposit() {
    let mut engine = Engine::new();
    deposit_and_dispute(&mut engine, 1, 1, "10.0000");

    engine
        .process(Transaction::Resolve { client: 1, tx: 1 })
        .unwrap();

    let acct = account_for(&engine, 1);
    assert_eq!(acct.available(), "10.0000".parse::<Decimal>().unwrap());
    assert_eq!(acct.held(), Decimal::ZERO);
}

#[test]
fn process_should_return_not_disputed_when_resolve_targets_undisputed_tx() {
    let mut engine = Engine::new();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "10.0000".parse().unwrap(),
        })
        .unwrap();

    let err = engine
        .process(Transaction::Resolve { client: 1, tx: 1 })
        .unwrap_err();

    assert!(matches!(err, EngineError::NotDisputed { client: 1, tx: 1 }));
}

#[test]
fn process_should_hold_funds_again_when_dispute_fires_after_resolve() {
    // Per Q5 a deposit may be re-disputed after resolve.
    let mut engine = Engine::new();
    deposit_and_dispute(&mut engine, 1, 1, "10.0000");
    engine
        .process(Transaction::Resolve { client: 1, tx: 1 })
        .unwrap();

    engine
        .process(Transaction::Dispute { client: 1, tx: 1 })
        .unwrap();

    let acct = account_for(&engine, 1);
    assert_eq!(acct.available(), Decimal::ZERO);
    assert_eq!(acct.held(), "10.0000".parse::<Decimal>().unwrap());
}

#[test]
fn process_should_lock_account_when_chargeback_targets_disputed_deposit() {
    let mut engine = Engine::new();
    deposit_and_dispute(&mut engine, 1, 1, "10.0000");

    engine
        .process(Transaction::Chargeback { client: 1, tx: 1 })
        .unwrap();

    let acct = account_for(&engine, 1);
    assert!(acct.locked());
    assert_eq!(acct.held(), Decimal::ZERO);
    assert_eq!(acct.available(), Decimal::ZERO);
}

#[test]
fn process_should_return_not_disputed_when_chargeback_repeats() {
    let mut engine = Engine::new();
    deposit_and_dispute(&mut engine, 1, 1, "10.0000");
    engine
        .process(Transaction::Chargeback { client: 1, tx: 1 })
        .unwrap();

    let err = engine
        .process(Transaction::Chargeback { client: 1, tx: 1 })
        .unwrap_err();

    assert!(matches!(err, EngineError::NotDisputed { client: 1, tx: 1 }));
}

#[test]
fn process_should_return_account_locked_when_deposit_targets_locked_account() {
    let mut engine = Engine::new();
    deposit_and_dispute(&mut engine, 1, 1, "10.0000");
    engine
        .process(Transaction::Chargeback { client: 1, tx: 1 })
        .unwrap();

    let err = engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 2,
            amount: "5.0000".parse().unwrap(),
        })
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::AccountLocked { client: 1, tx: 2 }
    ));
}

#[test]
fn process_should_release_held_when_resolve_targets_disputed_tx_on_locked_account() {
    // Per Q2 a resolve on a tx already in `Disputed` is allowed even on a
    // locked account: the dispute pre-dates the lock.
    let mut engine = Engine::new();
    deposit_and_dispute(&mut engine, 1, 1, "10.0000");
    deposit_and_dispute(&mut engine, 1, 2, "5.0000");
    engine
        .process(Transaction::Chargeback { client: 1, tx: 1 })
        .unwrap();

    engine
        .process(Transaction::Resolve { client: 1, tx: 2 })
        .unwrap();

    let acct = account_for(&engine, 1);
    assert_eq!(acct.held(), Decimal::ZERO);
    assert_eq!(acct.available(), "5.0000".parse::<Decimal>().unwrap());
    assert!(acct.locked());
}

#[test]
fn process_should_drive_total_negative_when_fraud_sequence_charges_back() {
    // Q3 fraud sequence: deposit 100, withdraw 80, dispute, chargeback.
    let mut engine = Engine::new();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "100.0000".parse().unwrap(),
        })
        .unwrap();
    engine
        .process(Transaction::Withdrawal {
            client: 1,
            tx: 2,
            amount: "80.0000".parse().unwrap(),
        })
        .unwrap();
    engine
        .process(Transaction::Dispute { client: 1, tx: 1 })
        .unwrap();

    engine
        .process(Transaction::Chargeback { client: 1, tx: 1 })
        .unwrap();

    let acct = account_for(&engine, 1);
    assert_eq!(acct.available(), "-80.0000".parse::<Decimal>().unwrap());
    assert_eq!(acct.total(), "-80.0000".parse::<Decimal>().unwrap());
    assert!(acct.locked());
}

#[test]
fn process_should_return_charged_back_when_dispute_targets_charged_back_tx() {
    let mut engine = Engine::new();
    deposit_and_dispute(&mut engine, 1, 1, "10.0000");
    engine
        .process(Transaction::Chargeback { client: 1, tx: 1 })
        .unwrap();

    let err = engine
        .process(Transaction::Dispute { client: 1, tx: 1 })
        .unwrap_err();

    assert!(matches!(err, EngineError::ChargedBack { client: 1, tx: 1 }));
}

#[test]
fn process_should_return_withdrawal_dispute_when_dispute_targets_withdrawal() {
    // Withdrawals are dedup'd via `seen_txs` only; there is no entry in
    // `deposits`, and the dispute path must surface `WithdrawalDispute`
    // rather than `TxNotFound`.
    let mut engine = Engine::new();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "10.0000".parse().unwrap(),
        })
        .unwrap();
    engine
        .process(Transaction::Withdrawal {
            client: 1,
            tx: 2,
            amount: "1.0000".parse().unwrap(),
        })
        .unwrap();

    let err = engine
        .process(Transaction::Dispute { client: 1, tx: 2 })
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::WithdrawalDispute { client: 1, tx: 2 }
    ));
}

#[test]
fn process_should_return_duplicate_tx_id_when_deposit_reuses_existing_deposit_id() {
    let mut engine = Engine::new();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "10.0000".parse().unwrap(),
        })
        .unwrap();

    let err = engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "5.0000".parse().unwrap(),
        })
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::DuplicateTxId { client: 1, tx: 1 }
    ));
}

#[test]
fn process_should_return_duplicate_tx_id_when_withdrawal_reuses_existing_deposit_id() {
    let mut engine = Engine::new();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "10.0000".parse().unwrap(),
        })
        .unwrap();

    let err = engine
        .process(Transaction::Withdrawal {
            client: 1,
            tx: 1,
            amount: "1.0000".parse().unwrap(),
        })
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::DuplicateTxId { client: 1, tx: 1 }
    ));
}

#[test]
fn process_should_return_duplicate_tx_id_when_deposit_reuses_existing_withdrawal_id() {
    let mut engine = Engine::new();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "10.0000".parse().unwrap(),
        })
        .unwrap();
    engine
        .process(Transaction::Withdrawal {
            client: 1,
            tx: 2,
            amount: "1.0000".parse().unwrap(),
        })
        .unwrap();

    let err = engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 2,
            amount: "5.0000".parse().unwrap(),
        })
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::DuplicateTxId { client: 1, tx: 2 }
    ));
}

#[test]
fn process_should_return_non_positive_amount_when_deposit_amount_is_negative() {
    let mut engine = Engine::new();

    let err = engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "-5.0000".parse().unwrap(),
        })
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::NonPositiveAmount {
            client: 1,
            tx: 1,
            ..
        }
    ));
}

#[test]
fn process_should_accept_subsequent_deposit_reusing_id_after_non_positive_rejection() {
    // Tx id stays free after a non-positive rejection per task 06; v2
    // matches v1 here so the corrected retry succeeds.
    let mut engine = Engine::new();

    let _ = engine.process(Transaction::Deposit {
        client: 1,
        tx: 1,
        amount: "-5.0000".parse().unwrap(),
    });
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "10.0000".parse().unwrap(),
        })
        .unwrap();

    assert_eq!(
        account_for(&engine, 1).available(),
        "10.0000".parse::<Decimal>().unwrap()
    );
}

#[test]
fn process_should_return_non_positive_amount_when_withdrawal_amount_is_zero() {
    let mut engine = Engine::new();
    engine
        .process(Transaction::Deposit {
            client: 1,
            tx: 1,
            amount: "10.0000".parse().unwrap(),
        })
        .unwrap();

    let err = engine
        .process(Transaction::Withdrawal {
            client: 1,
            tx: 2,
            amount: Decimal::ZERO,
        })
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::NonPositiveAmount {
            client: 1,
            tx: 2,
            ..
        }
    ));
}

#[test]
fn process_should_record_withdrawal_tx_id_even_when_insufficient_funds_rejects() {
    // Per 6a the failed withdrawal still consumes its tx id (recorded in
    // `seen_txs`), so a retry with the same id is flagged as duplicate.
    let mut engine = Engine::new();

    let _ = engine.process(Transaction::Withdrawal {
        client: 1,
        tx: 1,
        amount: "1.0000".parse().unwrap(),
    });

    let err = engine
        .process(Transaction::Withdrawal {
            client: 1,
            tx: 1,
            amount: "1.0000".parse().unwrap(),
        })
        .unwrap_err();

    assert!(matches!(
        err,
        EngineError::DuplicateTxId { client: 1, tx: 1 }
    ));
}
