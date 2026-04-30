//! Integration tests driving CSVs through `lib::run`.
//!
//! Snapshots are normalised by sorting data rows on `client` before
//! recording, so the spec's "output order is unconstrained" property
//! does not surface as snapshot churn.

use payments_engine_challenge::{run, run_v2};

fn normalise(raw: &str) -> String {
    let mut lines = raw.lines();
    let header = lines.next().unwrap_or_default();
    let mut rows: Vec<&str> = lines.collect();
    rows.sort();

    let mut normalised = String::with_capacity(raw.len());
    normalised.push_str(header);
    normalised.push('\n');
    for row in rows {
        normalised.push_str(row);
        normalised.push('\n');
    }
    normalised
}

/// Drive `input` through the default (v1) pipeline AND through v2,
/// asserting their byte-for-byte equality on the normalised output. The
/// returned string is v1's snapshot — v2 producing anything else would
/// have aborted the test before this point. Cross-checking on every
/// scenario gives the v1/v2 observable-contract invariant test coverage
/// without forcing a parallel snapshot tree.
fn run_and_normalise(input: &str) -> String {
    let mut v1_out = Vec::new();
    run(input.as_bytes(), &mut v1_out).unwrap();
    let v1 = normalise(&String::from_utf8(v1_out).unwrap());

    let mut v2_out = Vec::new();
    run_v2(input.as_bytes(), &mut v2_out).unwrap();
    let v2 = normalise(&String::from_utf8(v2_out).unwrap());

    assert_eq!(
        v1, v2,
        "engine v2 diverged from v1 on the same input — option A and option B must produce identical observable output",
    );
    v1
}

#[test]
fn deposits_only_should_emit_correct_balances_per_client() {
    let input = "\
type,client,tx,amount
deposit,1,1,1.0000
deposit,2,2,2.0000
deposit,1,3,2.0000
deposit,3,4,0.5000
deposit,2,5,0.1234
";

    insta::assert_snapshot!("deposits_only", run_and_normalise(input));
}

#[test]
fn deposit_with_one_decimal_should_render_as_four_decimal_places() {
    let input = "\
type,client,tx,amount
deposit,1,1,1.5
";

    insta::assert_snapshot!("deposit_one_decimal_renders_four", run_and_normalise(input));
}

#[test]
fn unknown_row_type_should_be_skipped_without_failing_the_pipeline() {
    let input = "\
type,client,tx,amount
deposit,1,1,1.0000
transfer,1,2,5.0000
deposit,1,3,2.0000
";

    insta::assert_snapshot!("unknown_row_skipped", run_and_normalise(input));
}

#[test]
fn deposit_then_dispute_should_hold_funds_in_snapshot() {
    // Client 1: deposits 10, then disputes that deposit. Funds move from
    // available to held; total stays at 10. Client 2: undisputed deposit.
    let input = "\
type,client,tx,amount
deposit,1,1,10.0000
deposit,2,2,5.0000
dispute,1,1,
";

    insta::assert_snapshot!("deposit_then_dispute", run_and_normalise(input));
}

#[test]
fn deposit_dispute_resolve_should_release_held_funds_in_snapshot() {
    // Client 1: deposits 10, disputes that deposit, resolves it. Funds move
    // from held back to available; snapshot matches the pre-dispute state.
    let input = "\
type,client,tx,amount
deposit,1,1,10.0000
dispute,1,1,
resolve,1,1,
";

    insta::assert_snapshot!("deposit_dispute_resolve", run_and_normalise(input));
}

#[test]
fn redispute_after_resolve_should_hold_funds_again_in_snapshot() {
    // Per Q5 a deposit may be re-disputed after resolve. End state has 10.0
    // held and 0 available, identical to a single-dispute run.
    let input = "\
type,client,tx,amount
deposit,1,1,10.0000
dispute,1,1,
resolve,1,1,
dispute,1,1,
";

    insta::assert_snapshot!("redispute_after_resolve", run_and_normalise(input));
}

#[test]
fn deposits_and_withdrawals_should_settle_to_expected_balances() {
    // Client 1: 10 deposited, 3.5 withdrawn, ends at 6.5.
    // Client 2: 4 deposited, attempted 9 withdrawal rejected (insufficient
    // funds), then 1.25 withdrawn, ends at 2.75.
    // Client 3: only a withdrawal attempt; account auto-created at zero,
    // withdrawal rejected, balances stay zero.
    let input = "\
type,client,tx,amount
deposit,1,1,10.0000
deposit,2,2,4.0000
withdrawal,1,3,3.5000
withdrawal,2,4,9.0000
withdrawal,3,5,1.0000
withdrawal,2,6,1.2500
";

    insta::assert_snapshot!("deposits_and_withdrawals", run_and_normalise(input));
}

#[test]
fn deposit_dispute_chargeback_should_lock_account_in_snapshot() {
    // Client 1: deposits 10, disputes that deposit, chargeback reverses it.
    // Held returns to 0, available stays 0, total drops to 0, account locked.
    let input = "\
type,client,tx,amount
deposit,1,1,10.0000
dispute,1,1,
chargeback,1,1,
";

    insta::assert_snapshot!("deposit_dispute_chargeback", run_and_normalise(input));
}

#[test]
fn fraud_sequence_should_drive_balance_negative_in_snapshot() {
    // Per Q3, a deposit that gets withdrawn before being charged back drives
    // total below zero, modelling the platform's exposure post-fraud.
    let input = "\
type,client,tx,amount
deposit,1,1,100.0000
withdrawal,1,2,80.0000
dispute,1,1,
chargeback,1,1,
";

    insta::assert_snapshot!("fraud_sequence_negative_balance", run_and_normalise(input));
}

#[test]
fn malformed_row_in_middle_should_skip_and_continue_in_snapshot() {
    // Row 2 has an unparseable amount; the csv layer rejects it. Per task 06
    // the driver loop downgrades that to `log::warn!` and the pipeline
    // continues, so the snapshot reflects rows 1 and 3 only.
    let input = "\
type,client,tx,amount
deposit,1,1,10.0000
deposit,1,2,not_a_number
deposit,1,3,2.5000
";

    insta::assert_snapshot!("malformed_row_skipped_continues", run_and_normalise(input));
}

#[test]
fn duplicate_tx_ids_should_apply_only_first_event_in_snapshot() {
    // Per 6a the second event reusing tx 1 is rejected. The cross-type
    // collision on tx 2 (deposit then withdrawal with the same id) leaves
    // the deposit's funds untouched.
    let input = "\
type,client,tx,amount
deposit,1,1,10.0000
deposit,1,1,99.0000
deposit,1,2,5.0000
withdrawal,1,2,3.0000
";

    insta::assert_snapshot!("duplicate_tx_ids_first_wins", run_and_normalise(input));
}

#[test]
fn mixed_ignorable_events_should_complete_without_crashing_in_snapshot() {
    // Drives all of TxNotFound (dispute on tx 99), DuplicateTxId (deposit
    // tx 1 twice), AccountLocked (deposit on the locked account), and
    // NotDisputed (resolve on a tx never disputed) through one CSV; the
    // pipeline never crashes and the snapshot captures the survivors.
    let input = "\
type,client,tx,amount
deposit,1,1,10.0000
deposit,1,1,5.0000
dispute,1,99,
deposit,2,2,4.0000
resolve,2,2,
dispute,1,1,
chargeback,1,1,
deposit,1,3,1.0000
";

    insta::assert_snapshot!("mixed_ignorable_events", run_and_normalise(input));
}

#[test]
fn non_positive_amounts_should_be_rejected_without_consuming_tx_ids_in_snapshot() {
    // Negative deposits and withdrawals are partner errors: a negative
    // deposit drives `available` negative without a chargeback, a negative
    // withdrawal credits the account by debiting a negative. Both rejected
    // up-front without consuming the tx id, so a corrected retry on the
    // same id (here tx 2 redeposited as +4) still applies.
    let input = "\
type,client,tx,amount
deposit,1,1,10.0000
deposit,1,2,-5.0000
withdrawal,1,3,-3.0000
deposit,1,2,4.0000
";

    insta::assert_snapshot!("non_positive_amounts_rejected", run_and_normalise(input));
}

#[test]
fn prior_dispute_resolve_should_still_succeed_after_lock_in_snapshot() {
    // Per Q2, a resolve on a tx already in `Disputed` state is allowed even
    // after a different dispute's chargeback locked the account. Client 1
    // has tx 1 (charged back, locks account) and tx 2 (disputed before lock,
    // resolved after).
    let input = "\
type,client,tx,amount
deposit,1,1,10.0000
deposit,1,2,5.0000
dispute,1,1,
dispute,1,2,
chargeback,1,1,
resolve,1,2,
";

    insta::assert_snapshot!("prior_dispute_resolve_after_lock", run_and_normalise(input));
}
