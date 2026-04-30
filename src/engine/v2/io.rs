//! v2's CSV entry point. Mirrors [`super::super::v1::io::run`] so both
//! variants share one driver and one writer (defined in
//! [`super::super::io`]) with only the engine constructor differing.

use std::io::{Read, Write};

use super::super::io::{drive_input, write_snapshots};
use super::Engine;

/// Read transactions from `input`, drive the v2 engine, then write the
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
