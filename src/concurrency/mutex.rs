//! `Arc<Mutex<Engine>>` variant. Wraps the production engine in one
//! global lock; producers serialize on `Mutex::lock` for every tx. The
//! contention upper bound: every successful submit pays a contended
//! atomic acquire even when the prior tx touched a different account.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use crate::bench_support::ProducerStream;
use crate::engine::Engine;
use crate::engine::account::Account;
use crate::engine::error::EngineError;
use crate::engine::io::{drive_input, write_output};
use crate::engine::transaction::Transaction;

use super::workload::{LatencyRecorder, WorkloadResult, sorted_accounts};

/// `Arc<Mutex<Engine>>` handle. `Clone` is intentionally on `Arc`
/// rather than the wrapper so callers see two distinct engine handles
/// can share the same underlying engine.
#[derive(Clone, Default)]
pub struct MutexEngine {
    engine: Arc<Mutex<Engine>>,
}

impl MutexEngine {
    /// Construct an empty handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one transaction. Acquires the global lock, calls
    /// [`Engine::process`], releases.
    ///
    /// # Errors
    /// Forwards every [`EngineError`] variant from [`Engine::process`].
    /// Panics if the lock is poisoned, which would imply a panic inside
    /// the engine (a bug, not a recoverable state).
    pub fn submit(&self, tx: Transaction) -> Result<(), EngineError> {
        let mut guard = self.engine.lock().expect("engine mutex poisoned");
        guard.process(tx)
    }

    /// Shared-state snapshot without consuming `self`. Used by the
    /// CSV-driven test entry point and the multi-producer bench
    /// drivers, both of which need the final accounts after every
    /// producer has dropped its handle.
    pub fn snapshot(&self) -> Vec<Account> {
        let guard = self.engine.lock().expect("engine mutex poisoned");
        sorted_accounts(guard.accounts().cloned())
    }
}

/// CSV-driven entry point for the test runner.
///
/// # Errors
/// Propagates structural IO failures from the CSV reader / writer.
pub fn run<R: Read, W: Write>(input: R, output: W) -> anyhow::Result<()> {
    let handle = MutexEngine::new();
    drive_input(input, |tx| handle.submit(tx))?;
    write_output(output, &handle.snapshot())
}

/// Multi-producer driver. Splits `streams` across `producers` worker
/// threads, each calling [`MutexEngine::submit`] for every tx in its
/// slice.
pub fn run_workload(producers: usize, streams: Vec<ProducerStream>) -> WorkloadResult {
    assert_eq!(
        streams.len(),
        producers,
        "stream count must match producer count",
    );
    let handle = MutexEngine::new();
    let bench_start = Instant::now();

    let mut join_handles = Vec::with_capacity(producers);
    for stream in streams {
        let producer_handle = handle.clone();
        join_handles.push(thread::spawn(move || {
            let mut recorder = LatencyRecorder::new();
            // Stamp inline (not upfront) so the recorded latency
            // captures only the lock-wait + apply cost of *this* tx,
            // not the cumulative producer-thread wall-time.
            for tx in stream.txs {
                let sent_at_ns = bench_start.elapsed().as_nanos() as u64;
                let _ = producer_handle.submit(tx);
                recorder.record(bench_start, sent_at_ns);
            }
            recorder
        }));
    }

    let mut recorders = Vec::with_capacity(producers);
    for h in join_handles {
        recorders.push(h.join().expect("producer thread panicked"));
    }

    WorkloadResult {
        accounts: handle.snapshot(),
        recorders,
    }
}
