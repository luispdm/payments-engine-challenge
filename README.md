# payments-engine-challenge

Solution to the payments engine challenge.

## Engine variants and benchmarks (task 07a)

Two storage layouts coexist under `src/engine/`:

- **v1**: single `HashMap<u32, TxRecord>` where `TxRecord` is an enum
  (`Deposit(DepositRecord) | Withdrawal`). One source of truth for both
  dispute lookup and tx-id dedup.
- **v2**: split layout. `HashMap<u32, DepositRecord>` for disputable
  deposits plus `HashSet<u32>` for cross-type id dedup.

Both expose the same observable contract (`process(Transaction) ->
Result<(), EngineError>` and an `accounts()` iterator); the integration
suite in `tests/scenarios.rs` runs every CSV scenario through both engines
and asserts byte-for-byte equivalence.

### How to reproduce

```sh
scripts/bench_summary.sh             # runs criterion + memory bins, prints
                                     # the markdown table below
scripts/bench_summary.sh --skip-bench # only re-emit the table from cached
                                     # criterion output + a fresh memory run
```

The harness sits behind a `bench` Cargo feature so the production build
carries no `rand`, `libc`, or `criterion` runtime dependency.

### Workload

Single-threaded. Deterministic `SmallRng` seeded with a constant. 10k
clients, tx mix 50% deposit / 30% withdrawal / 10% dispute / 7% resolve /
3% chargeback. Lifecycle events target a uniformly random already-emitted
deposit. 1M tx for the criterion throughput bench, 10M tx for the
one-shot memory bench. Same input feeds both variants, and a correctness
gate compares final account state across v1 and v2 before any timing.

### Results

Numbers below were captured on the development machine; rerun the script
locally to replace them with your own. Throughput is whole-pipeline
tx/sec on the 1M-tx workload; RSS is peak `ru_maxrss` from
`getrusage(RUSAGE_SELF)` after the 10M-tx workload has been ingested.

| Variant | Storage | 1M-tx mean ± stddev (ms) | Throughput (Mtx/s) | 10M-tx peak RSS (MiB) |
|---------|---------|--------------------------|--------------------|-----------------------|
| v1      | single `HashMap<u32, TxRecord>` | 100.75 ± 5.28 | 9.93 | 381.5 |
| v2      | `HashMap<u32, DepositRecord>` + `HashSet<u32>` | 84.34 ± 6.84 | 11.86 | 326.2 |

### Findings

- **v2 wins on both axes.** ~17% faster on throughput and ~14% smaller
  peak RSS at 10M tx. The dedup-only `HashSet<u32>` carries 4-byte keys
  vs v1's enum-typed `TxRecord` slot (24 bytes due to the
  `DepositRecord` payload), so withdrawals — 30% of the stream —
  consume far less memory per row.
- **The throughput delta tracks branch-prediction and cache locality.**
  v1's hot path discriminates `TxRecord::Deposit` vs
  `TxRecord::Withdrawal` on every dedup and dispute lookup; v2 hits the
  deposit map directly for lifecycle events and the set for dedup, with
  no enum tag indirection.
- **Trade-off is drift risk.** v2 holds two collections that have to
  stay in sync ("every deposit id in the map is also in the set"); the
  engine enforces this via a single insertion point, but a future
  refactor could break the invariant. v1 trades performance for a
  single source of truth. At MVP scale (well below 1B tx) both fit
  trivially; at 1B-tx scale neither does and the right answer is
  external storage.
- **Concurrency is a separate question.** Under a sharded lock (DashMap)
  v2 pays two shard-lock acquisitions per dispute path vs v1's one, so
  the relative cost can shift. Task 07b explores concurrency holding
  the storage variant fixed at v1's layout.

### Scope

Out of scope for 07a: concurrency (07b), external storage (architectural
change rather than a swap), `f64` amounts (correctness loss), `i64`
fixed-point (marginal at this scale), hasher swap, `Vec<Option<Account>>`
for accounts.
