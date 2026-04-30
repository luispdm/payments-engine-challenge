//! Engine variants. Versioned modules sit alongside one another so future
//! implementations (`v2`, `v3`, …) can be benchmarked without restructuring.
//!
//! The shared [`io`] module hosts the CSV driver and writer, parameterized
//! over a `process` closure so neither variant has to import the other.

pub mod io;
pub mod v1;
pub mod v2;
