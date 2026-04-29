//! Payments engine library entry point.
//!
//! Subsequent tasks bolt feature implementations onto the engine module
//! and wire them through [`run`].

use std::io::{Read, Write};

pub mod engine;

/// Process a CSV input stream and emit the per-client account snapshot to `output`.
///
/// Skeleton for task 00; later tasks parse `input`, drive the engine, and
/// serialize results to `output`.
pub fn run<R: Read, W: Write>(_input: R, _output: W) -> anyhow::Result<()> {
    Ok(())
}
