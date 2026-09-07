# Build Plan

Self-contained. Hand this to a coding session on its own; it restates the
constraints it depends on. Full rationale is in `02-internal-design.md`.

---

## Standing rules

These apply to every milestone and override convenience.

1. `ledger-core` has no dependency that performs IO, async, or clock access.
   Enforce in CI (deny-list on the dependency graph). Determinism is a
   compile-time property, not a discipline.
2. No floats anywhere. `#![forbid(unsafe_code)]` in `ledger-types` and
   `ledger-core`.
3. No wall clock in the state machine. Time arrives as `LogTime` on a command.
4. No `HashMap` iteration in anything affecting state or output ordering.
5. Every IO call goes through a trait so a simulator can replace it.
6. Balance store is a persistent/immutable map from the first commit. Not
   `HashMap`. This is not a later optimisation; converting afterwards is a
   rewrite.
7. Amounts are unsigned `u128` with direction in an explicit `Side` enum.
8. Zero-sum is enforced per denomination, never globally.

---

## Workspace

```
ledger/
  ledger-types/    money, ids, time. no logic.
  ledger-core/     state machine. pure.
  ledger-log/      record encoding + storage traits
  ledger-store/    real IO impl
  ledger-sim/      deterministic simulation harness
```

Create `ledger-types` and `ledger-core` only. The rest appear at the milestone
that needs them.

---

## M0 — Types (1–2 evenings)

**Build**

- `AccountId(u64)`, `AddressId(u32)`, `DenomId(u16)`, `ClientKey`
- `Money` / `Amount` with denomination enforced by the type system
- `Side { Debit, Credit }`, `Phase { Committed, PendingIn, PendingOut }`
- `BalanceKey { account, address, denom, phase }` — `Copy`, `Hash`, `Ord`
- `Balance { credit: i128, debit: i128 }`
- Three time types with no implicit conversion: `LogTime`, `ValueTime`,
  `BusinessDate`. Any conversion is a named function stating its policy.

**Done when**

- A `trybuild` compile-fail test proves USD + EUR does not compile.
- Rounding helpers exist and each names an explicit policy.

---

## M1 — State machine, in memory (the substantial one)

**Build**

```rust
struct Posting { key: BalanceKey, side: Side, amount: u128 }

struct Batch {
    idempotency_key: ClientKey,
    value_time:      ValueTime,
    postings:        Vec<Posting>,
}

fn apply(state: &State, cmd: &Command, at: LogTime)
    -> Result<(State, Effects), Rejection>;
```

- `State` holds balances in a persistent map (`imbl` is fine to start).
- Commands: `PostBatch`, `OpenAccount`, `CloseAccount`, `Tick`.
- Idempotency index inside `State`. Eviction keyed on log position, never wall
  clock. Same key with a different body is a hard rejection.
- `Rejection` as a stable, explicitly-numbered enum. Clients will branch on it,
  so treat the numbers as permanent from day one.
- Two-phase transfers via `Phase`: authorise, settle, release.
- Account closure requires all balance addresses at zero.

**Do not build**: persistence, contracts, scheduler, async, network.

**Done when**

- `proptest` over arbitrary command sequences holds per-denomination zero-sum.
- Closure-with-nonzero-balance is rejected.
- Replayed idempotency keys return the original outcome.
- A criterion bench gives a baseline number. Expect low hundreds of thousands
  of batches/sec single-threaded. Record it; it is the reference forever.
bench_apply - Sep 4, 2026:
  200000 iterations in 120.49125ms
  602.5 ns/op  (~1659872 batches/sec, single-threaded, debug-affected unless --release)
---

## M2 — Log and replay, still in memory

**Build**

- Versioned record encoding. Forward-compatible: fields may be added, never
  removed or reinterpreted.
- State machine version stamped in the log header.
- `log: Vec<Record>` and `replay(&[Record]) -> State`.

**Done when**

- Run N random commands through `apply`; replay the log from empty; assert the
  two states are byte-identical. This test justifies the entire architecture —
  do not proceed past it while it is red.

---

## M3 — Durability

**Build**

- Storage traits in `ledger-log`. The in-memory log from M2 becomes the test
  implementation for free.
- `ledger-store`: segment files, per-record checksum, explicit fsync, batched
  commit, torn-tail truncation on recovery.
- `ledger-sim` starts here — seeded PRNG, injected clock, fault injection at
  the storage trait boundary.

Build the simulator alongside this milestone, not after. This is the first
milestone with anything worth injecting faults into.

**Done when**

- Crash at an arbitrary injected point, recover, and the log is a valid prefix.
- Torn writes, short writes and bit flips are all detected.
- A failing seed reproduces exactly.

---

## M4 — Snapshots

**Build**

- Deterministic serialisation of `State`. Byte-identical across runs and
  processes; requires deterministic ordering throughout.
- Snapshot taken from an immutable root without stalling the writer (the
  persistent map gives this).

**Done when**

- `replay(snapshot + suffix) == replay(genesis)`, byte for byte.
- Two independent runs over the same log produce identical snapshot checksums.

---

## M5 — Read path

MVCC queries against retained roots. Bitemporal query: balance as of value date
X as known at time Y. Change stream for downstream consumers. Decide and
document what consistency an API caller gets immediately after a write.

---

## M6 — Scheduler

Log-time driven. `BTreeMap<(LogTime, AccountId), ScheduleId>`. Firings enter as
commands, never as direct calls from a timer.

Implement lazy accrual first: accrued interest as a pure function of balance
timeline and rate schedule, evaluated on read. Materialise postings only at
capitalisation. Add real scheduled events afterwards, for contracts that
declare themselves non-lazy.

---

## M7 — Contract host

wasmtime with fuel metering, memory cap, NaN canonicalisation, threads and SIMD
disabled, no ambient imports. Narrow ABI: read own balances, read parameters,
read log time, emit postings, reject with a code, request a schedule.

Contracts content-addressed; accounts record the hash. Migration is an explicit
logged command with a conversion hook.

Address manifests are enforced here: postings to undeclared addresses are
rejected.

---

## Not yet

Sharding, striped internal accounts, timing wheels, io_uring, custom
allocators, consensus, HTTP/gRPC surface. Each is a response to a measured
problem. Revisit only when the single-writer benchmark from M1 is genuinely the
ceiling.

---

## First session

`cargo new` the workspace, write `ledger-types`, get the compile-fail currency
test passing. That forces the money and identity decisions everything
downstream inherits.
