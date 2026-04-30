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
