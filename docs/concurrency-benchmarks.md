# Concurrency benchmarks

Comparison of five concurrency models for the payments engine. The
goal is to land production guidance for what to swap in if the
synchronous CSV pipeline ever needs to absorb thousands of concurrent
input streams. The losing variants are not on `main`; this document
preserves the result.

The data-structure layer was held constant at the production engine's
storage layout (the layout chosen by the data-structure benchmarks at
[`docs/data-structures-benchmarks.md`](data-structures-benchmarks.md));
this benchmark only varies the concurrency strategy.

## Variants

1. **baseline**: single-threaded engine. Throughput lower bound.
2. **mutex**: `Arc<Mutex<Engine>>`. One global lock around the whole
   engine. Contention upper bound.
3. **dashmap**: production engine's three collections (`accounts`,
   `deposits`, `seen_txs`) replaced with `DashMap` / `DashSet`.
   Disjoint clients don't block each other; same-client serializes on
   the shard's `RwLock`.
4. **actor_std**: one consumer thread owns the engine; producer threads
   enqueue `Transaction`s over `std::sync::mpsc::sync_channel(1024)`.
5. **actor_crossbeam**: same actor shape as #4 with
   `crossbeam::channel::bounded(1024)` instead of the standard library
   channel. Both are included to quantify whether `crossbeam-channel`
   earns its keep as a runtime dep.

`tokio::sync::mpsc` is deliberately excluded. With synchronous producer
threads it reduces to a channel-internals microbench whose answer is
known: tokio loses to crossbeam on overhead because it is designed for
async tasks, not sync threads. A fair tokio comparison would require
modeling realistic upstream I/O on the producer side, which turns the
microbench into a load test and conflates engine concurrency with
network stack behavior. Production deployment behind thousands of TCP
streams would put async I/O upstream of a synchronous engine consumer,
but that is a system-level architectural point, not a kernel
measurement.

## How to reproduce

The harness, all five engine variants, and the cross-variant
correctness gate live on
[`task/07b-benchmarking-concurrency`](https://github.com/luispdm/payments-engine-challenge/tree/task/07b-benchmarking-concurrency).
To rerun the numbers locally:

```sh
git checkout task/07b-benchmarking-concurrency
scripts/bench_summary.sh             # runs criterion + memory bins,
                                     # prints the markdown tables below
scripts/bench_summary.sh --skip-bench # only re-emit the tables from
                                     # cached criterion output + a
                                     # fresh memory run
```

The harness is gated behind a `bench` Cargo feature so the production
build carries no `dashmap`, `crossbeam`, or `hdrhistogram` runtime
dependency. Each per-variant `bench_*` binary reads
`BENCH_OVERLAP_PCT` (0..=100, default 100) and `BENCH_TX_COUNT`
(default 10M) so the summary script can sweep either axis across the
per-variant one-shot bins.

## Workload

Deterministic `SmallRng` seeded with a constant. 10k clients, tx mix
50% deposit / 30% withdrawal / 10% dispute / 7% resolve / 3%
chargeback (the same mix as the data-structure bench). Lifecycle
events target a uniformly random already-emitted deposit *from the
same producer's stream* so per-stream ordering preserves the engine's
deposit-before-dispute invariant.

The `throughput_1m` criterion bench drives 1M tx per iteration at
each of four overlap ratios. The `scaling_50ov` criterion bench
holds overlap at 50% and sweeps tx_count across `{100, 1k, 10k,
100k, 1M}`. The one-shot tail-latency / memory bins drive 10M tx by
default; the scaling table reuses them at smaller tx counts. Producer
count is pinned at 8. Actor channel capacity is pinned at 1024.
DashMap shard count is the library default
(`(num_cpus * 4).next_power_of_two()`); tuning DashMap is out of
scope.

### Latency definition

Apply latency is the per-tx delta between "producer stamp" and
"engine apply complete":

- **baseline / mutex / dashmap**: producer stamps `sent_at_ns` with
  `bench_start.elapsed()` immediately before `submit`; recording is
  done immediately after `submit` returns. The captured value is
  lock-wait + apply (or apply-only for baseline).
- **actor_std / actor_crossbeam**: producer stamps `sent_at_ns` on a
  channel envelope before `send`; the consumer thread records the
  delta after `engine.process` returns. The captured value is
  channel-enqueue + queue-wait + apply, which is what an actor
  client perceives end-to-end.

A previous iteration recorded actor latency on the producer side
right after `send`, which only captured channel-enqueue cost and
made the actor histograms incomparable to the sync variants.
Consumer-side stamping is the corrected measurement.

### Client overlap ratio

The swept axis is the fraction of every producer's draws that come
from a shared client pool versus that producer's private partition:

- **0%**: every producer touches a disjoint slice of clients. No two
  threads ever serialize on a shared account.
- **100%**: every producer can touch every client. Maximum
  account-level contention.
- **25% / 50%**: intermediate.

The cross-variant correctness gate runs at 0% overlap only. There the
final state is invariant to producer interleaving and every variant
must match the baseline byte-for-byte; a mismatch aborts the bench.
Higher overlap ratios skip strict equality because chargebacks on
shared accounts are order-sensitive (a chargeback locks the account,
after which a concurrent deposit on the same client is rejected
instead of accepted). That is the intrinsic cost of dropping the
global lock; engine-level invariants (`available + held == total`,
non-negative tx-id reservation) still hold.

## Throughput

Numbers below were captured on the development machine.

### 1M-tx mean ± stddev (ms) and throughput (Mtx/s) across overlap ratios

| Variant | ov0% (ms) | ov0% (Mtx/s) | ov25% (ms) | ov25% (Mtx/s) | ov50% (ms) | ov50% (Mtx/s) | ov100% (ms) | ov100% (Mtx/s) |
|---------|-----------|--------------|------------|---------------|------------|---------------|-------------|----------------|
| baseline | 150.67 ± 2.50 | 6.64 | 154.10 ± 2.88 | 6.49 | 155.20 ± 2.81 | 6.44 | 159.93 ± 6.81 | 6.25 |
| mutex | 503.82 ± 11.04 | 1.98 | 505.48 ± 11.26 | 1.98 | 495.76 ± 6.76 | 2.02 | 498.06 ± 7.75 | 2.01 |
| dashmap | 58.90 ± 0.73 | 16.98 | 57.95 ± 1.43 | 17.26 | 57.29 ± 1.77 | 17.46 | 58.39 ± 1.24 | 17.13 |
| actor_std | 6461.76 ± 129.66 | 0.15 | 6432.52 ± 137.77 | 0.16 | 6818.72 ± 181.49 | 0.15 | 6041.73 ± 496.47 | 0.17 |
| actor_crossbeam | 636.88 ± 126.34 | 1.57 | 535.09 ± 31.56 | 1.87 | 690.32 ± 153.66 | 1.45 | 381.54 ± 28.03 | 2.62 |

## Tail latency and peak resident memory

10M tx per cell, captured by the per-variant one-shot bench
binaries swept across the four overlap ratios.

| Variant | Overlap | p50 (µs) | p90 (µs) | p99 (µs) | Peak RSS (MiB) |
|---------|---------|----------|----------|----------|----------------|
| baseline | 0% | 0.0 | 0.2 | 0.3 | 269.2 |
| baseline | 25% | 0.0 | 0.2 | 0.4 | 269.2 |
| baseline | 50% | 0.0 | 0.2 | 0.3 | 269.2 |
| baseline | 100% | 0.1 | 0.2 | 0.3 | 282.5 |
| mutex | 0% | 0.5 | 6.7 | 25.8 | 339.4 |
| mutex | 25% | 0.4 | 6.7 | 25.5 | 336.9 |
| mutex | 50% | 0.4 | 6.4 | 25.7 | 338.8 |
| mutex | 100% | 0.5 | 6.8 | 26.4 | 338.1 |
| dashmap | 0% | 0.1 | 0.3 | 0.7 | 315.4 |
| dashmap | 25% | 0.1 | 0.3 | 0.7 | 313.4 |
| dashmap | 50% | 0.1 | 0.4 | 0.7 | 307.1 |
| dashmap | 100% | 0.1 | 0.4 | 0.7 | 309.1 |
| actor_std | 0% | 147.5 | 7352.3 | 10674.2 | 327.3 |
| actor_std | 25% | 158.3 | 7286.8 | 10559.5 | 327.4 |
| actor_std | 50% | 154.9 | 7254.0 | 10436.6 | 327.3 |
| actor_std | 100% | 32.7 | 7217.2 | 10395.6 | 327.0 |
| actor_crossbeam | 0% | 20.9 | 404.2 | 1771.5 | 327.4 |
| actor_crossbeam | 25% | 76.5 | 369.7 | 1531.9 | 327.4 |
| actor_crossbeam | 50% | 13.8 | 331.8 | 1426.4 | 327.3 |
| actor_crossbeam | 100% | 11.2 | 280.8 | 1283.1 | 327.4 |

## Scaling sweep at 50% overlap

How throughput and tail latency scale with workload size, at a fixed
50% client overlap. Each row is one
`(variant, tx_count)` cell; throughput from the criterion
`scaling_50ov` group, percentiles from the matching one-shot bin.
RSS is omitted because at small tx counts it just reflects the bin's
startup pages.

The `Iter (ms)` column is the wall-clock time per criterion
iteration (drains all `tx_count` transactions). The
`p50 / p90 / p99 (µs)` columns are per-tx apply-latency
percentiles. The two have different denominators (whole batch vs
one tx); for actor variants per-tx percentiles can be orders of
magnitude larger than `Iter / tx_count` because each tx waits in
the bounded channel behind the others.

| Variant | tx_count | Throughput (Mtx/s) | Iter (ms) | p50 (µs) | p90 (µs) | p99 (µs) |
|---------|----------|--------------------|-----------|----------|----------|----------|
| baseline | 100 | 6.99 | 0.01 | 0.1 | 0.1 | 4.7 |
| baseline | 1000 | 7.49 | 0.13 | 0.1 | 0.1 | 1.1 |
| baseline | 10000 | 7.47 | 1.34 | 0.1 | 0.1 | 0.2 |
| baseline | 100000 | 8.62 | 11.61 | 0.1 | 0.1 | 0.2 |
| baseline | 1000000 | 6.82 | 146.72 | 0.1 | 0.1 | 0.3 |
| mutex | 100 | 0.27 | 0.38 | 0.1 | 0.8 | 3.8 |
| mutex | 1000 | 2.10 | 0.48 | 0.1 | 0.3 | 12.3 |
| mutex | 10000 | 2.03 | 4.93 | 0.3 | 2.7 | 59.4 |
| mutex | 100000 | 2.08 | 48.08 | 0.5 | 9.4 | 45.1 |
| mutex | 1000000 | 2.02 | 494.12 | 0.7 | 10.7 | 39.2 |
| dashmap | 100 | 0.26 | 0.39 | 0.2 | 1.1 | 2.9 |
| dashmap | 1000 | 2.03 | 0.49 | 0.2 | 0.4 | 3.3 |
| dashmap | 10000 | 7.79 | 1.28 | 0.2 | 0.5 | 5.5 |
| dashmap | 100000 | 14.25 | 7.02 | 0.3 | 0.5 | 3.5 |
| dashmap | 1000000 | 17.77 | 56.28 | 0.2 | 0.5 | 0.9 |
| actor_std | 100 | 0.18 | 0.57 | 12.1 | 55.3 | 58.3 |
| actor_std | 1000 | 1.63 | 0.61 | 67.1 | 120.3 | 139.4 |
| actor_std | 10000 | 0.38 | 25.99 | 4534.3 | 12255.2 | 15646.7 |
| actor_std | 100000 | 0.46 | 216.03 | 171.5 | 6615.0 | 9936.9 |
| actor_std | 1000000 | 0.17 | 5991.25 | 6115.3 | 9257.0 | 12419.1 |
| actor_crossbeam | 100 | 0.19 | 0.53 | 21.4 | 132.9 | 136.3 |
| actor_crossbeam | 1000 | 1.80 | 0.56 | 25.2 | 46.7 | 61.3 |
| actor_crossbeam | 10000 | 1.81 | 5.54 | 726.0 | 3301.4 | 4579.3 |
| actor_crossbeam | 100000 | 3.75 | 26.65 | 94.5 | 730.6 | 2318.3 |
| actor_crossbeam | 1000000 | 1.70 | 588.08 | 175.0 | 628.2 | 2775.0 |

### Scaling-curve observations

- **baseline is flat across scales** (~7 Mtx/s, ~100 ns p99). No
  threading, so there is no startup amortization or contention
  signal. The single number characterizes the variant at every
  workload size.
- **dashmap shows clear amortization.** Throughput climbs
  monotonically from 0.26 Mtx/s at 100 tx to 17.77 Mtx/s at 1M tx,
  a ~70x improvement. Producer-thread spawn cost dominates the
  smallest cell; by 100k tx that fixed cost has been spread thin and
  the variant's parallel apply is the headline. The scaling curve
  argues for batching CSV input into chunks ≥ 100k tx in any
  pipeline using this variant.
- **mutex hits a plateau at ~2 Mtx/s by 1k tx** and stays there.
  The fixed thread-spawn overhead is amortized away early, but the
  global lock then caps throughput regardless of workload size. Tail
  latency creeps up with scale (p99 = 12µs at 1k tx, 39µs at 1M tx)
  as the wait queue builds.
- **Actor variants show a queueing knee.** Both actors finish
  roughly even at 1k tx (~1.6 / 1.8 Mtx/s), but `actor_std` collapses
  past 10k tx as the std channel's lock contends under sustained
  back-pressure: throughput at 10k tx is only 0.38 Mtx/s with p50
  jumping to 4.5 ms. `actor_crossbeam` keeps a steadier curve and
  peaks at 100k tx (3.75 Mtx/s) before tailing slightly at 1M.
- **Anomaly: dashmap's small-scale throughput is below baseline.** At
  100 / 1k tx, dashmap (0.26 / 2.03 Mtx/s) is slower than baseline
  (~7 Mtx/s) because the variant spawns 8 producer threads to push
  100 tx, thread spawn cost dwarfs the work. Below ~10k tx,
  serial baseline is the better choice; above, dashmap wins
  decisively.

## Findings

- **DashMap wins decisively, ~2.7x over baseline at every overlap.**
  Sharded locking lets the eight producer threads progress in
  parallel for any pair of accesses landing on different shards;
  same-shard accesses serialize on the shard's `RwLock`, which is
  the same primitive the global mutex variant uses but applied at
  finer granularity. p99 latency stays under a microsecond at every
  overlap, well within the noise floor for the apply cost itself.
  The scaling sweep adds a caveat: dashmap only beats baseline at
  workloads ≥ 10k tx; smaller batches are better served by the
  single-threaded engine.
- **Mutex underperforms baseline.** The single-threaded baseline does
  no atomic work between txs; `Arc<Mutex<Engine>>` serializes on a
  contended atomic acquire for every submit and produces sub-2 Mtx/s
  vs ~6.5 Mtx/s baseline. The eight producer threads are not adding
  parallelism here, only contention: the engine's apply work is
  short enough that the mutex's wait-and-wake cost dominates.
- **Actor variants pay the channel + serial-consumer tax.** With the
  consumer single-threaded by design, the channel adds queueing on
  top of the same per-tx work the baseline does, and now that
  latency is recorded on the consumer side it captures the
  end-to-end queue wait, not just enqueue cost. `actor_std`
  collapses to ~0.15 Mtx/s (4x slower than mutex, 40x slower than
  dashmap); `actor_crossbeam` is ~1.5-2.6 Mtx/s, comparable to
  mutex. The scaling sweep shows `actor_std` falls off a cliff past
  10k tx as the std channel's internal lock saturates;
  `actor_crossbeam` plateaus instead of collapsing.
- **`crossbeam-channel` is dramatically faster than `std::sync::mpsc`
  under back-pressure.** Same actor shape, same workload; only the
  channel implementation differs. Throughput at 1M tx is ~10x
  better. Crossbeam's lock-free design avoids the std lock
  contention every send takes when the consumer is the bottleneck.
  For any actor-shaped deployment the dep is worth its weight.
- **Throughput is largely insensitive to overlap ratio.** All
  non-actor variants measure within ~10% across overlap=0% and
  overlap=100%. This says the workload's hot path is dominated by
  the per-tx engine apply work, not by any contention specific to
  shared accounts. The actor variants' variance is large enough to
  swamp any overlap signal; the consumer's queue length, not the
  client overlap, dominates their numbers.
- **Memory.** All five variants land within ~25% of each other's peak
  RSS at 10M tx. The mutex and actor variants pay an extra ~50-70 MiB
  over baseline / DashMap, mostly from the producer threads' stacks
  and the consumer's per-tx histogram bucket fills. DashMap's
  per-shard arrays add a small fixed cost over baseline. None of
  these are deal-breakers at the spec's scale.

### Recommendation

For the workload assumed here (case B, many concurrent streams across
overlapping clients, sync engine consumer), **DashMap is the swap
worth landing** *for batches large enough to amortize producer-thread
spawn cost* (≥ 10k tx). Roughly 2.7x throughput over the
single-threaded baseline at 1M tx, sub-microsecond p99 even at 100%
overlap, and only ~30 MiB of extra peak RSS at 10M tx. The engine's
existing `process` shape changes from `&mut self` to `&self`, and
the lock-ordering discipline (`deposits → accounts`, never the
reverse) needs to be preserved across future edits. For small
batches the single-threaded baseline remains the better choice; the
scaling sweep argues for a batch threshold in any production
pipeline.

The actor approach is viable when the engine consumer must remain
strictly serial for some external reason (e.g., write-ahead logging,
deterministic replay), in which case `crossbeam-channel` is the
right channel to pick. The standard-library channel collapses past
~10k tx and is unsuitable for any sustained workload.

## Scope

Out of scope here:

- Data-structure / concurrency cross product. All five variants run on
  the production storage layout; the data-structure-versus-DashMap
  question was settled at
  [`docs/data-structures-benchmarks.md`](data-structures-benchmarks.md).
- Real-network producer load. Different methodology (load test, not
  microbench) and conflates engine concurrency with the network stack.
- Async producer model. See "Tokio deliberately excluded" above.
- DashMap shard-count tuning. Library default; optimizing shard count
  for this specific workload is its own benchmark.
- `f64` amounts. Ruled out for correctness.
- `i64` fixed-point. Marginal at this scale, maintenance burden.
- Hasher swap. Known ecosystem fact, would lift every variant
  uniformly.
- `Vec<Option<Account>>` for accounts. Marginal at our scale.
