//! CSV serde glue between the driver loop and the engine.
//!
//! Reading streams rows lazily (no full buffering of input) so the engine
//! can in principle consume arbitrarily large inputs. Writing emits
//! amounts at exactly 4 decimal places per decision Q6b.
//!
//! Partner-error handling per task 06: per-row CSV deserialize failures
//! (unparseable amount, type-mismatched fields, …) and engine errors that
//! the spec instructs us to ignore are downgraded to `log::warn!` and the
//! pipeline continues. IO failures at the underlying reader propagate via
//! `anyhow::Context` because they are not row-local; recovery is the
//! caller's job.

use std::io::{Read, Write};

use anyhow::Context;

use super::Engine;
use super::transaction::{RawTransaction, Transaction};

/// Read transactions from `input`, drive `engine`, then write the
/// resulting account snapshots to `output`.
///
/// Per-row partner errors (unknown row type, missing amount, malformed
/// fields, engine validations) emit `log::warn!` and the pipeline
/// advances. Structural failures at the IO or CSV-framework level
/// propagate through `anyhow`.
///
/// # Errors
///
/// Propagates structural IO failures from the underlying readers and
/// writers.
pub fn run<R: Read, W: Write>(input: R, output: W) -> anyhow::Result<()> {
    let mut engine = Engine::new();
    read_into(&mut engine, input)?;
    write_snapshots(&engine, output)
}

fn read_into<R: Read>(engine: &mut Engine, input: R) -> anyhow::Result<()> {
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
        if let Err(err) = engine.process(tx) {
            log::warn!("ignoring transaction: {err}");
        }
    }
    Ok(())
}

fn write_snapshots<W: Write>(engine: &Engine, output: W) -> anyhow::Result<()> {
    let mut wtr = csv::Writer::from_writer(output);
    wtr.write_record(["client", "available", "held", "total", "locked"])?;
    for acct in engine.accounts() {
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
        read_into(&mut engine, input.as_bytes()).unwrap();
        engine
    }

    #[test]
    fn read_should_tolerate_whitespace_around_fields() {
        let engine = run_csv(
            "  type ,  client , tx , amount\n\
             deposit ,  1 , 1 ,  10.0000 \n",
        );

        let acct = engine.accounts().next().unwrap();
        assert_eq!(acct.available(), "10.0000".parse::<Decimal>().unwrap());
    }

    #[test]
    fn read_should_preserve_four_decimal_precision_exactly() {
        let engine = run_csv(
            "type,client,tx,amount\n\
             deposit,1,1,0.0001\n",
        );

        let acct = engine.accounts().next().unwrap();
        assert_eq!(acct.available(), "0.0001".parse::<Decimal>().unwrap());
    }

    #[test]
    fn read_should_skip_unknown_row_type() {
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
    fn read_should_skip_deposit_with_missing_amount() {
        let engine = run_csv(
            "type,client,tx,amount\n\
             deposit,1,1,\n\
             deposit,1,2,4.0000\n",
        );

        let acct = engine.accounts().next().unwrap();
        assert_eq!(acct.available(), "4.0000".parse::<Decimal>().unwrap());
    }

    #[test]
    fn write_should_format_amounts_to_four_decimal_places() {
        let mut engine = Engine::new();
        engine
            .process(Transaction::Deposit {
                client: 1,
                tx: 1,
                amount: "1.5".parse().unwrap(),
            })
            .unwrap();

        let mut buf = Vec::new();
        write_snapshots(&engine, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();

        assert!(out.contains("1.5000"), "output was: {out}");
    }

    #[test]
    fn write_should_emit_header_row() {
        let engine = Engine::new();

        let mut buf = Vec::new();
        write_snapshots(&engine, &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();

        assert_eq!(out, "client,available,held,total,locked\n");
    }
}
