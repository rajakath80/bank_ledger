# Internal Design

Technical design paper. Companion to `01-external-why.md`.

---

## 1. Principles

1. The state machine is a pure function. `apply(state, command, log_time)`.
   No IO, no clock, no randomness, no async, no floats.
2. The ledger knows accounts, balances, postings and zero-sum. It does not know
   what interest, a mortgage, or a fee is.
3. IO is injected through traits everywhere, from the first commit, so the
   whole system can run inside a deterministic simulator.
4. Anything that cannot be retrofitted is built now. Anything that is a
   response to a measured problem waits until the problem is measured.

## 2. Crate boundaries

```
ledger-types/   money, identifiers, time. no logic.
ledger-core/    state machine. pure.
ledger-log/     record encoding + storage traits
ledger-store/   real IO (segment files, fsync)
ledger-sim/     deterministic simulation harness
ledger-api/     edges (much later)
```

`ledger-core` must not depend on any crate performing IO, async or clock
access. This is enforced in CI, not by discipline. It converts determinism from
a property someone has to maintain into a property the compiler checks.

## 3. Data model

An account does not have a balance. It has a map of addressable balances:

```rust
struct BalanceKey {
    account: AccountId,   // u64
    address: AddressId,   // u32, interned
    denom:   DenomId,     // u16
    phase:   Phase,       // Committed | PendingIn | PendingOut
}

struct Balance { credit: i128, debit: i128 }
```

`address` is the product-defined dimension: `DEFAULT`, `PRINCIPAL`,
`ACCRUED_INTEREST_RECEIVABLE`, `PENALTIES`. The ledger assigns no meaning.
Adding a product requires no schema change.

Addresses are interned `u32`, not strings. Contracts declare their address set
in a manifest; the host rejects postings to undeclared addresses. A typo
becomes a rejection rather than a phantom balance discovered at reconciliation.

Credit and debit are held separately rather than netted. Gross turnover is
required for reporting and reconciliation and cannot be recovered from a net
figure.

`phase` is part of the key, so two-phase authorisation lives in the data model
rather than a side table. Authorisation moves value to `PendingOut`; settlement
moves it to `Committed`; release moves it back. Available balance is a
projection over phases that the contract defines.

```rust
struct Posting {
    key:    BalanceKey,
    side:   Side,       // Debit | Credit
    amount: u128,       // unsigned; direction lives in `side`
}

struct Batch {
    idempotency_key: ClientKey,
    value_time:      ValueTime,
    postings:        Vec<Posting>,
}
```

Unsigned amount plus explicit side eliminates the "negative credit" class of
bug at the type level.

## 4. Money and identity

Integer minor units, `i128` internally. Currency carried in the type or as a
checked `DenomId`; adding USD to EUR must not compile. Every rounding operation
names a policy and every residual is posted to a real account, so the trial
balance still nets to zero.

Zero-sum is enforced **per denomination**, not globally. This forces FX to be
modelled as two single-currency legs against an FX position pair, which is the
honest representation.

## 5. State machine

```rust
fn apply(state: &State, cmd: &Command, at: LogTime)
    -> Result<(State, Effects), Rejection>;
```

Returning a new state rather than mutating is deliberate. The balance store is
a persistent map (HAMT) from the first commit, which gives copy-on-write
snapshots and MVCC reads later at no additional cost. Starting with `HashMap`
and converting afterwards is a rewrite of everything touching state.

Idempotency and the rejection code enum live inside the state machine, so both
are present from the beginning rather than bolted on.

**Dedup index.** Idempotency keys are unbounded deterministic state. Eviction
must be driven by log time or log position, never wall clock, or replay breaks.
A retry arriving with the same key but a different body is a hard rejection,
not a silent replay.

**Validation is account-local.** Pre-posting checks may read only the account's
own balances. This constraint looks like a limitation and is the reason
validation shards cleanly. Cross-account requirements (offset mortgages, group
limits) go through a coarser supervisor path. The constraint must not erode.

**Restrictions** — freezes, debit blocks, regulatory holds — are enforced in
the ledger fast path, not in contracts, because they must apply even when
product logic permits the transaction.

## 6. Time

Three distinct types, no implicit conversion:

- `LogTime` — monotonic, from committed tick commands. The only clock the state
  machine sees.
- `ValueTime` — economic effect date.
- `BusinessDate` — jurisdiction-specific, with a cutoff. Not a UTC date.

Every posting carries value time and booking time. Bitemporal queries are
"balance as of value date X, as known at time Y". Backdated corrections are new
postings, never mutations. A backdated posting may invalidate already-executed
schedules; the resolution is compensating adjustment postings rather than
schedule replay.

## 7. Log, versioning, replay

The log is the source of truth; balances are a projection.

Every record ever written must remain decodable by every future version of the
code, which is a stronger constraint than a normal wire format where old
messages age out. Records are versioned; fields may be added, never removed or
reinterpreted.

The harder problem: fixing a bug in the state machine changes what replay
produces. A state machine version is stamped in the log, and either old
behaviour is preserved behind a branch or there is an explicit snapshot-and-
freeze cutover after which prior history is never replayed.

The defining test: run N random commands, replay the resulting log from empty,
assert both states are byte-identical.

## 8. Snapshots

Two non-obvious requirements. Snapshots must be byte-identical across replicas
given the same log prefix, so replica divergence is detectable by checksum —
which forces deterministic ordering throughout the state representation. And
they must be taken without stalling the writer, which the persistent map from
section 5 provides directly.

## 9. Durability

Segment files, checksum per record, explicit fsync, batched commit. Recovery
truncates a torn tail. Assume fsync may lie; checksum everything.

Throughput is bound by durability round-trips, so batching is the primary
performance lever. Thousands of postings per fsync.

## 10. Scheduler

Schedule firings enter the log as commands. A wall-clock timer calling into the
state machine directly destroys replay determinism.

Default strategy is **lazy accrual**: accrued interest is a pure function of
`(balance timeline, rate schedule, now)`, evaluated on read. Postings are
materialised only at capitalisation. This removes the per-account nightly
thundering herd entirely. Contracts requiring conditions that cannot be
expressed lazily declare themselves non-lazy and receive real scheduled events.

Storage: `BTreeMap<(LogTime, AccountId), ScheduleId>`. A hierarchical timing
wheel only if that is measurably the bottleneck.

## 11. Contract host

WASM via wasmtime. Product logic is imperative and stateful, so a combinator
algebra is the wrong shape here.

Deterministic configuration: fuel metering, memory cap, NaN canonicalisation,
threads and SIMD disabled, no ambient imports. Host ABI is narrow — read own
balances, read parameters, read log time, emit postings, reject with a code,
request a schedule.

Contracts are content-addressed. Accounts record the hash they run. Migration
is an explicit logged event with a conversion hook. Because every input is
explicit, contract execution is replayable and covered by simulation tests.

## 12. Read path

Roughly half the system, and usually the half that receives no design
attention. Balance queries, bitemporal queries, statement extraction, and a
change stream for downstream consumers.

The copy-on-write state gives MVCC reads directly: a long statement query holds
an old root and never blocks the writer.

## 13. Testing

Deterministic simulation testing is the payoff for everything above. Seeded
PRNG drives scheduling, partitions, disk faults, crashes and clock skew.
Failures reproduce from a seed. Fault injection happens at the storage and
network trait boundaries.

Alongside it, property tests on the domain invariants: per-denomination
zero-sum, configured non-negativity, replay equivalence, snapshot determinism.

## 14. Deferred until measured

Sharding, striped internal accounts, timing wheels, io_uring, custom
allocators, consensus, API surface.

Note on striping: internal accounts (interest expense, fee income, settlement)
are touched by nearly every posting. On a single writer this is harmless. Under
sharding they become the shard everything queues behind, and the fix is fanning
one logical account out to N physical stripes selected by hash. This shapes the
account ID scheme, so it is designed now and built later.

Bitemporality is the one exception to deferral: value time and booking time go
on `Posting` immediately, even though nothing reads value time for months.
