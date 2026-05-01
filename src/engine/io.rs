//! CSV serde glue.
//!
//! The driver is parameterized over a `process` closure rather than a
//! method on [`super::Engine`] so concurrency variants can plug in
//! easily.
//!
//! Partner errors stay row-local: per-row CSV deserialize
//! failures and engine errors that the spec instructs to ignore are
//! logged via `log::warn!` and the pipeline continues. Underlying IO
//! failures propagate through `anyhow::Context` because they are not
//! row-local: recovery is the caller's job.

use std::io::{Read, Write};

use anyhow::Context;

use super::Engine;
use super::account::Account;
use super::error::EngineError;
use super::transaction::{RawTransaction, Transaction};

/// Read transactions from `input`, drive the engine, then write the
/// resulting account snapshots to `output`.
///
/// # Errors
///
/// Propagates structural IO failures from the underlying readers and
/// writers; per-row partner errors are logged and skipped.
pub fn run<R: Read, W: Write>(input: R, output: W) -> anyhow::Result<()> {
    let mut engine = Engine::new();
    drive_input(input, |tx| engine.process(tx))?;
    write_snapshots(output, engine.accounts())
}

/// Stream rows from `input` and feed each parsed [`Transaction`] to
/// `process`. Per-row partner errors emit `log::warn!`; structural IO
/// failures bubble up.
///
/// # Errors
///
/// Propagates structural IO failures from the underlying reader.
pub fn drive_input<R, P>(input: R, mut process: P) -> anyhow::Result<()>
where
    R: Read,
    P: FnMut(Transaction) -> Result<(), EngineError>,
{
    let mut rdr = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .flexible(true)
        .from_reader(input);

    for result in rdr.deserialize::<RawTransaction>() {
        let raw = match result {
            Ok(raw) => raw,
            Err(err) => {
                // IO errors below the csv layer are terminal: the input
                // reader itself is gone and we have nothing left to drain.
                // Per-row deserialize / framing errors are local to the bad
                // row and the loop carries on.
                if matches!(err.kind(), csv::ErrorKind::Io(_)) {
                    return Err(err).context("read CSV row");
                }
                log::warn!("skipping malformed CSV row: {err}");
                continue;
            }
        };
        let tx = match Transaction::try_from(raw) {
            Ok(tx) => tx,
            Err(err) => {
                log::warn!("skipping unprocessable row: {err}");
                continue;
            }
        };
        if let Err(err) = process(tx) {
            log::warn!("ignoring transaction: {err}");
        }
    }
    Ok(())
}

/// Write the per-client snapshot to `output` with amounts at exactly four
/// decimal places (Q6b).
///
/// # Errors
///
/// Propagates failures from the underlying writer.
pub fn write_snapshots<'a, W, I>(output: W, accounts: I) -> anyhow::Result<()>
where
    W: Write,
    I: IntoIterator<Item = &'a Account>,
{
    let mut wtr = csv::Writer::from_writer(output);
    wtr.write_record(["client", "available", "held", "total", "locked"])?;
    for acct in accounts {
        wtr.write_record([
            acct.client().to_string(),
            format!("{:.4}", acct.available()),
            format!("{:.4}", acct.held()),
            format!("{:.4}", acct.total()),
            acct.locked().to_string(),
        ])?;
    }
    wtr.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;

    fn run_csv(input: &str) -> Engine {
        let mut engine = Engine::new();
        drive_input(input.as_bytes(), |tx| engine.process(tx)).unwrap();
        engine
    }

    #[test]
    fn drive_input_should_tolerate_whitespace_around_fields() {
        let engine = run_csv(
            "  type ,  client , tx , amount\n\
             deposit ,  1 , 1 ,  10.0000 \n",
        );

        let acct = engine.accounts().next().unwrap();
        assert_eq!(acct.available(), "10.0000".parse::<Decimal>().unwrap());
    }

    #[test]
    fn drive_input_should_preserve_four_decimal_precision_exactly() {
        let engine = run_csv(
            "type,client,tx,amount\n\
             deposit,1,1,0.0001\n",
        );

        let acct = engine.accounts().next().unwrap();
        assert_eq!(acct.available(), "0.0001".parse::<Decimal>().unwrap());
    }

    #[test]
    fn drive_input_should_skip_unknown_row_type() {
        let engine = run_csv(
            "type,client,tx,amount\n\
             transfer,1,1,5.0000\n\
             deposit,1,2,3.0000\n",
        );

        let acct = engine.accounts().next().unwrap();
        assert_eq!(acct.client(), 1);
        assert_eq!(acct.available(), "3.0000".parse::<Decimal>().unwrap());
    }

    #[test]
    fn drive_input_should_skip_deposit_with_missing_amount() {
        let engine = run_csv(
            "type,client,tx,amount\n\
             deposit,1,1,\n\
             deposit,1,2,4.0000\n",
        );

        let acct = engine.accounts().next().unwrap();
        assert_eq!(acct.available(), "4.0000".parse::<Decimal>().unwrap());
    }

    #[test]
    fn write_snapshots_should_format_amounts_to_four_decimal_places() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "1.5".parse().unwrap(),
            })
            .unwrap();

        let mut buf = Vec::new();
        write_snapshots(&mut buf, engine.accounts()).unwrap();
        let out = String::from_utf8(buf).unwrap();

        assert!(out.contains("1.5000"), "output was: {out}");
    }

    #[test]
    fn write_snapshots_should_emit_header_row() {
        let engine = Engine::new();

        let mut buf = Vec::new();
        write_snapshots(&mut buf, engine.accounts()).unwrap();
        let out = String::from_utf8(buf).unwrap();

        assert_eq!(out, "client,available,held,total,locked\n");
    }
}
