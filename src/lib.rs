//! Payments engine library entry point.
//!
//! The default engine ships under [`engine::v1`]; alternative variants
//! (currently [`engine::v2`]) sit alongside so they can be benchmarked in
//! parallel. Test reuse follows the closure-parameterized
//! [`engine::io::drive_input`] helper rather than a trait so the engines
//! stay concrete types per `~/payments-engine-challenge-docs/decisions.md`.

use std::io::{Read, Write};

pub mod engine;

#[cfg(feature = "bench")]
pub mod bench_support;

#[cfg(feature = "bench")]
pub mod mem;

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

/// Same as [`run`] but routes through [`engine::v2::io::run`]. Exposed for
/// integration tests and benchmark drivers that need to exercise v2 over
/// the same input path; production callers should keep using [`run`].
///
/// # Errors
///
/// See [`run`].
pub fn run_v2<R: Read, W: Write>(input: R, output: W) -> anyhow::Result<()> {
    engine::v2::io::run(input, output)
}
