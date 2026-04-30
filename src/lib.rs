//! Payments engine library entry point.
//!
//! The current implementation lives under [`engine::v1`]; future engine
//! variants slot in as sibling modules so they can be benchmarked against
//! the baseline without restructuring callers.

use std::io::{Read, Write};

pub mod engine;

/// Process a CSV input stream and emit the per-client account snapshot to `output`.
///
/// Delegates to [`engine::v1::io::run`].
///
/// # Errors
///
/// Returns an error if the CSV input or output streams fail at the IO or
/// parse layer; partner errors in individual rows are silently skipped.
pub fn run<R: Read, W: Write>(input: R, output: W) -> anyhow::Result<()> {
    engine::v1::io::run(input, output)
}
