//! Single-threaded baseline. Wraps the production [`crate::engine::Engine`]
//! in the variant contract so the bench harness and test runner can drive
//! it through the same shape as the concurrent variants.
//!
//! No locking, no channels: the lower bound the other variants are
//! measured against. The multi-producer `run_workload` entry point ignores
//! its `producers` argument and consumes the whole workload sequentially
//! on the calling thread.

use std::io::{Read, Write};

use crate::bench_support::ProducerStream;
use crate::engine::Engine;
use crate::engine::account::Account;
use crate::engine::error::EngineError;
use crate::engine::io::{drive_input, write_output};
use crate::engine::transaction::Transaction;

use super::workload::{LatencyRecorder, WorkloadResult, sorted_accounts};

/// Single-threaded engine handle.
#[derive(Default)]
pub struct Baseline {
    engine: Engine,
}

impl Baseline {
    /// Construct an empty baseline handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one transaction. Mirrors [`Engine::process`] one-to-one.
    ///
    /// # Errors
    /// Forwards every variant of [`EngineError`] from [`Engine::process`].
    pub fn submit(&mut self, tx: Transaction) -> Result<(), EngineError> {
        self.engine.process(tx)
    }

    /// Drop the handle and return a deterministic, client-sorted account
    /// snapshot.
    pub fn finalize(self) -> Vec<Account> {
        sorted_accounts(self.engine.accounts().cloned())
    }
}

/// CSV-driven entry point for the test runner. Same signature as
/// [`crate::engine::io::run`]; tests dispatch to this function via the
/// shared scenario harness.
///
/// # Errors
/// Propagates structural IO failures from the CSV reader / writer.
pub fn run<R: Read, W: Write>(input: R, output: W) -> anyhow::Result<()> {
    let mut handle = Baseline::new();
    drive_input(input, |tx| handle.submit(tx))?;
    write_output(output, &handle.engine_accounts_snapshot())
}

impl Baseline {
    /// Helper for [`run`]: snapshot accounts as a `Vec<Account>` so the
    /// writer's trait bound (`IntoIterator<Item = &Account>`) is satisfied
    /// without exposing the underlying `HashMap`'s value iterator type.
    fn engine_accounts_snapshot(&self) -> Vec<Account> {
        self.engine.accounts().cloned().collect()
    }
}

/// Single-threaded driver. Baseline ignores `_producers` and drains
/// the flattened workload on the calling thread; the per-stream split
/// is used purely so the contract matches the multi-producer variants.
///
/// In the multi-producer variants the latency recorder captures the
/// per-tx submit cost (lock or channel send + apply work). Single-
/// threaded baseline has no queueing, so the recorded value is the
/// per-tx apply work only: each tx is stamped immediately before
/// `submit`, and the recorder reads `bench_start.elapsed()` right
/// after.
pub fn run_workload(_producers: usize, streams: Vec<ProducerStream>) -> WorkloadResult {
    let mut handle = Baseline::new();
    let mut recorder = LatencyRecorder::new();
    let bench_start = std::time::Instant::now();

    for stream in streams {
        for tx in stream.txs {
            let pre = bench_start.elapsed().as_nanos() as u64;
            let _ = handle.submit(tx);
            recorder.record(bench_start, pre);
        }
    }

    WorkloadResult {
        accounts: handle.finalize(),
        recorders: vec![recorder],
    }
}
