//! Actor variant backed by `crossbeam::channel::bounded`.
//!
//! Same shape as [`super::actor_std`]; both are included so the
//! benchmark can quantify the std-vs-crossbeam delta and either justify
//! adopting `crossbeam-channel` as a runtime dep or stick with the
//! standard library.
//!
//! Apply latency is recorded on the consumer side after
//! [`Engine::process`] returns, for the same reason as `actor_std`:
//! producer-side timing would only capture channel-enqueue cost.

use std::io::{Read, Write};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crossbeam::channel::{self, Sender};

use crate::bench_support::{CHANNEL_CAPACITY, ProducerStream};
use crate::engine::Engine;
use crate::engine::account::Account;
use crate::engine::error::EngineError;
use crate::engine::io::{drive_input, write_output};
use crate::engine::transaction::Transaction;

use super::workload::{BenchEnvelope, LatencyRecorder, WorkloadResult, sorted_accounts};

/// Actor handle backed by a `crossbeam::channel::bounded` pair.
pub struct ActorCrossbeamEngine {
    sender: Option<Sender<BenchEnvelope>>,
    consumer: Option<JoinHandle<(Engine, LatencyRecorder)>>,
    bench_start: Instant,
}

impl Default for ActorCrossbeamEngine {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}

impl ActorCrossbeamEngine {
    /// Spawn the consumer and return a producer handle. See
    /// [`super::actor_std::ActorStdEngine::new`] for the
    /// `bench_start` semantics.
    pub fn new(bench_start: Instant) -> Self {
        let (sender, receiver) = channel::bounded::<BenchEnvelope>(CHANNEL_CAPACITY);
        let consumer = thread::spawn(move || {
            let mut engine = Engine::new();
            let mut recorder = LatencyRecorder::new();
            while let Ok(env) = receiver.recv() {
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

    /// Clone the inner sender for a producer thread.
    pub fn sender(&self) -> Sender<BenchEnvelope> {
        self.sender
            .as_ref()
            .expect("actor sender already dropped")
            .clone()
    }

    /// Bench epoch the consumer measures `now` against.
    pub fn bench_start(&self) -> Instant {
        self.bench_start
    }

    /// Enqueue one transaction. Blocks when the channel is full.
    ///
    /// # Errors
    /// Always returns `Ok(())` while the actor is alive.
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
    pub fn finalize(mut self) -> Vec<Account> {
        let (engine, _) = self.join_consumer();
        sorted_accounts(engine.accounts().cloned())
    }

    /// Drop the producer side, join the consumer, return account
    /// snapshot + consumer-side latency recorder. Bench drivers use this.
    pub fn finalize_with_latency(mut self) -> (Vec<Account>, LatencyRecorder) {
        let (engine, recorder) = self.join_consumer();
        (sorted_accounts(engine.accounts().cloned()), recorder)
    }

    fn join_consumer(&mut self) -> (Engine, LatencyRecorder) {
        drop(self.sender.take());
        self.consumer
            .take()
            .expect("consumer already joined")
            .join()
            .expect("consumer thread panicked")
    }
}

impl Drop for ActorCrossbeamEngine {
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
    let handle = ActorCrossbeamEngine::new(Instant::now());
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
    let actor = ActorCrossbeamEngine::new(bench_start);

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
