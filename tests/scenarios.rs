//! Integration tests driving CSVs through `lib::run`.
//!
//! Snapshots are normalised by sorting data rows on `client` before
//! recording, so the spec's "output order is unconstrained" property does
//! not surface as snapshot churn.
//!
//! With `--features bench`, every scenario also runs through each of
//! the five concurrency variants and asserts byte-equality with the
//! production engine's output. That satisfies task 07b's "every variant
//! must pass the unit + integration test suite from tasks 01-06
//! unchanged" requirement: the integration tests here are the suite,
//! and the cross-variant assert is a single function each scenario
//! calls.

use payments_engine_challenge::run;

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

fn run_and_normalise(input: &str) -> String {
    let oracle = run_and_normalise_with(input, |i, o| run(i, o));
    cross_variant_check(input, &oracle);
    oracle
}

fn run_and_normalise_with<R>(input: &str, run_variant: R) -> String
where
    R: FnOnce(&[u8], &mut Vec<u8>) -> anyhow::Result<()>,
{
    let mut out = Vec::new();
    run_variant(input.as_bytes(), &mut out).unwrap();
    normalise(&String::from_utf8(out).unwrap())
}

#[cfg(not(feature = "bench"))]
fn cross_variant_check(_input: &str, _oracle: &str) {
    // Concurrency variants live behind the `bench` feature; without it
    // we only exercise the production engine.
}

#[cfg(feature = "bench")]
type VariantRunner = fn(&[u8], &mut Vec<u8>) -> anyhow::Result<()>;

#[cfg(feature = "bench")]
fn cross_variant_check(input: &str, oracle: &str) {
    use payments_engine_challenge::concurrency::{
        actor_crossbeam, actor_std, baseline, dashmap_engine, mutex,
    };

    let cases: [(&str, VariantRunner); 5] = [
        ("baseline", |i, o| baseline::run(i, o)),
        ("mutex", |i, o| mutex::run(i, o)),
        ("dashmap", |i, o| dashmap_engine::run(i, o)),
        ("actor_std", |i, o| actor_std::run(i, o)),
        ("actor_crossbeam", |i, o| actor_crossbeam::run(i, o)),
    ];
    for (name, run_fn) in cases {
        let got = run_and_normalise_with(input, run_fn);
        assert_eq!(
            got, oracle,
            "concurrency variant `{name}` diverged from production engine output",
        );
    }
}

#[test]
fn deposits_only_should_emit_correct_balances_per_client() {
    let input = include_str!("samples/deposits_only.csv");

    insta::assert_snapshot!("deposits_only", run_and_normalise(input));
}

#[test]
fn deposit_with_one_decimal_should_render_as_four_decimal_places() {
    let input = include_str!("samples/deposit_one_decimal_renders_four.csv");

    insta::assert_snapshot!("deposit_one_decimal_renders_four", run_and_normalise(input));
}

#[test]
fn unknown_row_type_should_be_skipped_without_failing_the_pipeline() {
    let input = include_str!("samples/unknown_row_skipped.csv");

    insta::assert_snapshot!("unknown_row_skipped", run_and_normalise(input));
}

#[test]
fn deposit_then_dispute_should_hold_funds_in_snapshot() {
    // Client 1: deposits 10, then disputes that deposit. Funds move from
    // available to held; total stays at 10. Client 2: undisputed deposit.
    let input = include_str!("samples/deposit_then_dispute.csv");

    insta::assert_snapshot!("deposit_then_dispute", run_and_normalise(input));
}

#[test]
fn deposit_dispute_resolve_should_release_held_funds_in_snapshot() {
    // Client 1: deposits 10, disputes that deposit, resolves it. Funds move
    // from held back to available; snapshot matches the pre-dispute state.
    let input = include_str!("samples/deposit_dispute_resolve.csv");

    insta::assert_snapshot!("deposit_dispute_resolve", run_and_normalise(input));
}

#[test]
fn redispute_after_resolve_should_hold_funds_again_in_snapshot() {
    // A deposit may be re-disputed after resolve. End state has 10.0
    // held and 0 available, identical to a single-dispute run.
    let input = include_str!("samples/redispute_after_resolve.csv");

    insta::assert_snapshot!("redispute_after_resolve", run_and_normalise(input));
}

#[test]
fn deposits_and_withdrawals_should_settle_to_expected_balances() {
    // Client 1: 10 deposited, 3.5 withdrawn, ends at 6.5.
    // Client 2: 4 deposited, attempted 9 withdrawal rejected (insufficient
    // funds), then 1.25 withdrawn, ends at 2.75.
    // Client 3: only a withdrawal attempt; account auto-created at zero,
    // withdrawal rejected, balances stay zero.
    let input = include_str!("samples/deposits_and_withdrawals.csv");

    insta::assert_snapshot!("deposits_and_withdrawals", run_and_normalise(input));
}

#[test]
fn deposit_dispute_chargeback_should_lock_account_in_snapshot() {
    // Client 1: deposits 10, disputes that deposit, chargeback reverses it.
    // Held returns to 0, available stays 0, total drops to 0, account locked.
    let input = include_str!("samples/deposit_dispute_chargeback.csv");

    insta::assert_snapshot!("deposit_dispute_chargeback", run_and_normalise(input));
}

#[test]
fn fraud_sequence_should_drive_balance_negative_in_snapshot() {
    // A deposit that gets withdrawn before being charged back drives
    // total below zero, modelling the platform's exposure post-fraud.
    let input = include_str!("samples/fraud_sequence_negative_balance.csv");

    insta::assert_snapshot!("fraud_sequence_negative_balance", run_and_normalise(input));
}

#[test]
fn malformed_row_in_middle_should_skip_and_continue_in_snapshot() {
    // Row 2 has an unparseable amount; the csv layer rejects it.
    // The driver loop downgrades that to `log::warn!` and the pipeline
    // continues, so the snapshot reflects rows 1 and 3 only.
    let input = include_str!("samples/malformed_row_skipped_continues.csv");

    insta::assert_snapshot!("malformed_row_skipped_continues", run_and_normalise(input));
}

#[test]
fn duplicate_tx_ids_should_apply_only_first_event_in_snapshot() {
    // The second event reusing tx 1 is rejected. The cross-type
    // collision on tx 2 (deposit then withdrawal with the same id) leaves
    // the deposit's funds untouched.
    let input = include_str!("samples/duplicate_tx_ids_first_wins.csv");

    insta::assert_snapshot!("duplicate_tx_ids_first_wins", run_and_normalise(input));
}

#[test]
fn mixed_ignorable_events_should_complete_without_crashing_in_snapshot() {
    // Drives all of TxNotFound (dispute on tx 99), DuplicateTxId (deposit
    // tx 1 twice), AccountLocked (deposit on the locked account), and
    // NotDisputed (resolve on a tx never disputed) through one CSV; the
    // pipeline never crashes and the snapshot captures the survivors.
    let input = include_str!("samples/mixed_ignorable_events.csv");

    insta::assert_snapshot!("mixed_ignorable_events", run_and_normalise(input));
}

#[test]
fn non_positive_amounts_should_be_rejected_without_consuming_tx_ids_in_snapshot() {
    // Negative deposits and withdrawals are partner errors: a negative
    // deposit drives `available` negative without a chargeback, a negative
    // withdrawal credits the account by debiting a negative. Both rejected
    // up-front without consuming the tx id, so a corrected retry on the
    // same id (here tx 2 redeposited as +4) still applies.
    let input = include_str!("samples/non_positive_amounts_rejected.csv");

    insta::assert_snapshot!("non_positive_amounts_rejected", run_and_normalise(input));
}

#[test]
fn prior_dispute_resolve_should_still_succeed_after_lock_in_snapshot() {
    // A resolve on a tx already in `Disputed` state is allowed even
    // after a different dispute's chargeback locked the account. Client 1
    // has tx 1 (charged back, locks account) and tx 2 (disputed before lock,
    // resolved after).
    let input = include_str!("samples/prior_dispute_resolve_after_lock.csv");

    insta::assert_snapshot!("prior_dispute_resolve_after_lock", run_and_normalise(input));
}
