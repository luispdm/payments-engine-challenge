//! Integration tests driving CSVs through `lib::run`.
//!
//! Snapshots are normalised by sorting data rows on `client` before
//! recording, so the spec's "output order is unconstrained" property
//! does not surface as snapshot churn.

use payments_engine_challenge::run;

fn run_and_normalise(input: &str) -> String {
    let mut output = Vec::new();
    run(input.as_bytes(), &mut output).unwrap();
    let raw = String::from_utf8(output).unwrap();

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
