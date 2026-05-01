//! Payments engine library entrypoint.

use std::io::{Read, Write};

pub mod engine;

/// Process a CSV input stream and emit the per-client account snapshot to `output`.
///
/// Delegates to [`engine::io::run`].
///
/// # Errors
///
/// Returns an error if the CSV input or output streams fail at the IO or
/// parse layer. Errors in individual rows are silently skipped.
pub fn run<R: Read, W: Write>(input: R, output: W) -> anyhow::Result<()> {
    engine::io::run(input, output)
}
