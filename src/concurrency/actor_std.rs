//! Actor variant backed by `std::sync::mpsc::sync_channel`.
//!
//! One consumer thread owns the engine; producer threads send
//! [`BenchEnvelope`]s over a bounded channel. Channel capacity 1024 is
//! pinned per task 07b; under back-pressure the producer's `send`
//! blocks rather than dropping work or growing the buffer.
//!
//! Apply latency is recorded on the **consumer** side after
//! [`Engine::process`] returns. Recording on the producer side would
//! capture only the channel-enqueue cost (since `send` returns as soon
//! as the slot is taken) and would be incomparable across variants.
//! Each producer stamps `sent_at_ns` on the envelope; the consumer
//! samples `bench_start.elapsed()` after `process` and records the
//! difference. The CSV-driven `run` path constructs envelopes too but
//! discards the consumer's recorder via [`ActorStdEngine::finalize`].

use std::io::{Read, Write};
use std::sync::mpsc::{self, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::bench_support::{CHANNEL_CAPACITY, ProducerStream};
use crate::engine::Engine;
use crate::engine::account::Account;
use crate::engine::error::EngineError;
use crate::engine::io::{drive_input, write_output};
use crate::engine::transaction::Transaction;

use super::workload::{BenchEnvelope, LatencyRecorder, WorkloadResult, sorted_accounts};

/// Actor handle. Owns the producer-side of the bounded channel and the
/// consumer's join handle. Both fields are `Option`-wrapped so
/// `finalize` and `Drop` can take them without leaving an inconsistent
/// `self`.
pub struct ActorStdEngine {
    sender: Option<SyncSender<BenchEnvelope>>,
    consumer: Option<JoinHandle<(Engine, LatencyRecorder)>>,
    bench_start: Instant,
}

impl Default for ActorStdEngine {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}

impl ActorStdEngine {
    /// Spawn the consumer thread and hand back the producer handle.
    ///
    /// `bench_start` is the shared epoch the producers stamp their
    /// `sent_at_ns` against. CSV callers pass `Instant::now()` and
    /// ignore the consumer-side recorder via [`Self::finalize`].
    pub fn new(bench_start: Instant) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<BenchEnvelope>(CHANNEL_CAPACITY);
        let consumer = thread::spawn(move || {
            let mut engine = Engine::new();
            let mut recorder = LatencyRecorder::new();
            for env in receiver {
                let _ = engine.process(env.tx);
                recorder.record(bench_start, env.sent_at_ns);
            }
            (engine, recorder)
        });
        Self {
            sender: Some(sender),
            consumer: Some(consumer),
            bench_start,
        }
    }

    /// Clone the inner sender. Used by the multi-producer bench driver
    /// to hand each producer its own send handle. Panics if the actor
    /// has already been finalized.
    pub fn sender(&self) -> SyncSender<BenchEnvelope> {
        self.sender
            .as_ref()
            .expect("actor sender already dropped")
            .clone()
    }

    /// Bench epoch the consumer measures `now` against. Producers
    /// stamp envelopes with `bench_start.elapsed().as_nanos() as u64`
    /// so both sides agree on the time origin.
    pub fn bench_start(&self) -> Instant {
        self.bench_start
    }

    /// Enqueue one transaction. Blocks when the channel is full.
    /// Producer-side latency stamping is unused on this path because
    /// the consumer always records.
    ///
    /// # Errors
    /// Always returns `Ok(())` while the actor is alive; engine errors
    /// raised by the consumer are swallowed. Panics if the consumer has
    /// died (would imply an engine bug, since the consumer never panics
    /// on engine errors).
    pub fn submit(&self, tx: Transaction) -> Result<(), EngineError> {
        let envelope = BenchEnvelope { tx, sent_at_ns: 0 };
        self.sender
            .as_ref()
            .expect("actor sender already dropped")
            .send(envelope)
            .expect("actor consumer terminated unexpectedly");
        Ok(())
    }

    /// Drop the producer side, join the consumer, return its accounts.
    /// Discards the consumer-side recorder; CSV path uses this.
    pub fn finalize(mut self) -> Vec<Account> {
        let (engine, _) = self.join_consumer();
        sorted_accounts(engine.accounts().cloned())
    }

    /// Drop the producer side, join the consumer, return both the
    /// account snapshot and the consumer-side latency recorder. Bench
    /// drivers use this.
    pub fn finalize_with_latency(mut self) -> (Vec<Account>, LatencyRecorder) {
        let (engine, recorder) = self.join_consumer();
        (sorted_accounts(engine.accounts().cloned()), recorder)
    }

    fn join_consumer(&mut self) -> (Engine, LatencyRecorder) {
        // Drop sender so the consumer's `for env in receiver` loop exits.
        drop(self.sender.take());
        self.consumer
            .take()
            .expect("consumer already joined")
            .join()
            .expect("consumer thread panicked")
    }
}

impl Drop for ActorStdEngine {
    fn drop(&mut self) {
        if let Some(handle) = self.consumer.take() {
            drop(self.sender.take());
            let _ = handle.join();
        }
    }
}

/// CSV-driven entry point for the test runner.
///
/// # Errors
/// Propagates structural IO failures from the CSV reader / writer.
pub fn run<R: Read, W: Write>(input: R, output: W) -> anyhow::Result<()> {
    let handle = ActorStdEngine::new(Instant::now());
    drive_input(input, |tx| handle.submit(tx))?;
    let accounts = handle.finalize();
    write_output(output, &accounts)
}

/// Multi-producer bench driver.
pub fn run_workload(producers: usize, streams: Vec<ProducerStream>) -> WorkloadResult {
    assert_eq!(
        streams.len(),
        producers,
        "stream count must match producer count",
    );
    let bench_start = Instant::now();
    let actor = ActorStdEngine::new(bench_start);

    let mut join_handles = Vec::with_capacity(producers);
    for stream in streams {
        let producer_sender = actor.sender();
        join_handles.push(thread::spawn(move || {
            for tx in stream.txs {
                let sent_at_ns = bench_start.elapsed().as_nanos() as u64;
                producer_sender
                    .send(BenchEnvelope { tx, sent_at_ns })
                    .expect("actor consumer terminated mid-bench");
            }
        }));
    }

    for h in join_handles {
        h.join().expect("producer thread panicked");
    }

    let (accounts, recorder) = actor.finalize_with_latency();
    WorkloadResult {
        accounts,
        recorders: vec![recorder],
    }
}
