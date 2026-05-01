# payments-engine-challenge

Rust solution to the payments engine challenge. CSV in, per-client
account snapshot CSV out, dispute lifecycle implemented end to end.

## AI USAGE DISCLAIMER

This challenge has been completed with the assistance of Claude Code.
The AI tool has been used during both the design and development phase.

## Build, run, test

```sh
cargo build --release
cargo run --release -- transactions.csv > accounts.csv
cargo test
cargo doc --document-private-items --open # check out the full project doc
```

## Assumptions

- **Disputes target deposits only.** A dispute, resolve, or chargeback
  pointing at a withdrawal is ignored. The dispute math
  (decrease available, increase held, total unchanged) only models
  un-crediting a deposit: applying it to a withdrawal would double-debit
  the client.
- **Locked accounts reject new client activity but settle pending
  disputes.** A locked account refuses deposits, withdrawals, and *new*
  disputes. Resolves and chargebacks on disputes opened *before* the
  lock, do keep the account locked but still process, so legitimate
  cleanup of pre-fraud disputes can finish.
- **Negative balances are allowed.** A chargeback on a deposit whose
  funds were already withdrawn drives `available` and `total` negative.
- **Three-state dispute lifecycle.**
  `NotDisputed → Disputed → NotDisputed` (resolve) or
  `Disputed → ChargedBack` (chargeback). A resolved tx may be disputed
  again. A double-dispute on an already-disputed tx is idempotent. A
  charged-back tx is non-reversible. This might be different in
  real-world finance, but for the purposes of this challenge it is done
  to avoid overcomplicating the state machine.
- **Defensive cross-type tx-id dedup.** Every tx id seen (deposit and
  withdrawal alike) is recorded. A second event reusing the same id is
  rejected. When a negative withdrawal or deposit is received, the tx
  id is not stored: this is treated as an accidental error from the
  caller (like a 400 in HTTP world), and not a genuine attempt.
- **Output amounts always render with four decimal places.**

## Benchmarks

Benchmarks ran on an AMD Ryzen 7 7900 and 32 GB of RAM.

Two benchmark suites probed the design space: the conclusions shaped
the production engine.

The data-structure bench compared a single `HashMap<u32, TxRecord>`
enum-tagged ledger against a split
`HashMap<u32, DepositRecord> + HashSet<u32>` layout. The split layout
is ~22% faster and ~14% smaller at 10M tx: it is what runs on `main`.

The concurrency bench compared five strategie: single-threaded,
`Arc<Mutex<Engine>>`, DashMap-backed, std-mpsc actor,
and crossbeam-channel actor at four client-overlap ratios.
DashMap wins decisively at workloads ≥ 10k tx (~2.7x baseline,
sub-microsecond p99 at every overlap): below that scale, the
single-threaded baseline is preferable because thread spawn cost
dominates the work.

For further details, check:
- [Data structure storage layouts](docs/data-structures-benchmarks.md)
- [Concurrency models](docs/concurrency-benchmarks.md)
- [Data structure bench code](https://github.com/luispdm/payments-engine-challenge/pull/8)
- [Concurrency bench code](https://github.com/luispdm/payments-engine-challenge/pull/9)

## Known limitations

- **Single-threaded engine on `main`.** In production, a sharded app on
  `client_id` spread on many instances would be a better fit.
- **All state in memory.** Account state and every tx id
  ever seen are kept in process memory. Again, I wouldn't do this in
  production. In a real-world scenario I would offload this
  to a key-value DB, or if the situation allows it, a bloom filter
  backed by a DB lookup to rule out false positives.
- **No durability.** A crash mid-run loses partial state. This can be
  mitigated with different strategies, one being a write-ahead log.

## Potential improvements

- TCP server bundling to test async runtimes.
- clippy lint rules.
- `rstest` to for a better unit tests organization.
- macros to reduce the engine LoC.
