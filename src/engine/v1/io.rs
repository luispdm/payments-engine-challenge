//! CSV serde glue between the driver loop and the engine.
//!
//! Reading streams rows lazily (no full buffering of input) so the engine
//! can in principle consume arbitrarily large inputs. Writing emits
//! amounts at exactly 4 decimal places per decision Q6b.

use std::io::{Read, Write};

use super::Engine;
use super::transaction::{RawTransaction, Transaction};

/// Read transactions from `input`, drive `engine`, then write the
/// resulting account snapshots to `output`.
///
/// Partner errors (unknown row type, missing amount, future engine
/// validation failures) are silently skipped at this stage; task 06
/// upgrades them to `log::warn!` once the logging facade is wired up.
///
/// # Errors
///
/// Propagates structural IO and CSV parse errors from the underlying
/// readers and writers.
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
        let raw = result?;
        let Ok(tx) = Transaction::try_from(raw) else {
            continue;
        };
        let _ = engine.process(tx);
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
