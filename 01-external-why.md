# A Deterministic Ledger Core for Programmable Banking

External design paper. Status: personal project, not commercial.

---

## 1. What a core banking system actually is

Strip away the marketing and a core banking system does three things: it holds
balances, it moves value between them under rules, and it can prove afterwards
that it did so correctly. Everything else — onboarding, cards, statements,
mobile apps — sits on top and can be replaced independently.

The system proposed here is only the first three things, done properly. The
central claim is that the ledger should be small, dumb, and extremely fast, and
that all product behaviour should live above it as versioned, sandboxed
contracts. Almost every problem described below traces back to systems that
blur that line.

## 2. Where current systems fall short

**Generation one: mainframe cores.** COBOL on IBM z/OS, still running a large
share of the world's retail deposits. They are reliable and fast in a narrow
sense, but product logic is compiled into the core. Launching a new savings
product is a change-managed release measured in quarters. Balances are
maintained by an overnight batch, which is why the industry still has a "batch
window", why transfers pend overnight, and why a batch overrun is a customer-
visible incident. History is not reconstructible; corrections are applied by
adjustment entries whose rationale lives in someone's spreadsheet.

**Generation two: cloud cores.** Thought Machine Vault, Mambu, 10x. These got
the most important thing right: the ledger stopped knowing what a mortgage is,
and products became configuration or code. That was the correct insight and
this design keeps it. But the implementations inherit three problems.

*Throughput is bound by a general-purpose database.* Postings land in
PostgreSQL or similar. Every posting has a counterparty, and the counterparty
for interest, fees and settlement is a small set of internal accounts. Those
rows become a lock-contention hotspot that no amount of horizontal scaling
fixes. Real-world throughput lands in the low thousands of postings per second.

*Scheduling is per-account cron.* Ten million accounts accruing interest at
midnight means ten million scheduled executions queued at one instant. The
thundering herd is managed operationally rather than designed away, and it is
the most common source of production pain in these systems.

*Execution is not deterministic.* Product logic runs in a general-purpose
language with ambient access to wall clock, network and randomness. The
consequence is that you cannot replay history and get the same answer, so you
cannot reconstruct state from the log, cannot compare two replicas by
checksum, and cannot test cluster behaviour under partition and crash by
simulation. Correctness is established by integration testing and hope.

**Blockchain and smart contract platforms.** These get determinism and
replayability right, and get almost everything else wrong for this domain.
There is no double-entry model, no bitemporality, no concept of value date
versus booking date, no chart of accounts, and consensus overhead is paid in a
setting where the participants are a single institution's own replicas. They
solve a trust problem that a bank's internal ledger does not have.

## 3. The thesis

A core ledger should be a deterministic replicated state machine.

State is a pure function of an ordered log of commands. No wall clock, no
randomness, no ambient IO inside the state machine. Time enters as a field on a
committed command. Given that, four properties fall out almost for free that
are extremely hard to add later:

- State at any past instant is reconstructible by replay, so audit is a
  computation rather than an archive.
- Two replicas that have applied the same log prefix must produce identical
  bytes, so divergence is detectable by checksum.
- The whole cluster can be run inside a simulation with a seeded random number
  generator driving thread scheduling, network partitions, disk faults and
  clock skew. Failures reproduce exactly from a seed.
- Product logic, executing under the same rules, becomes replayable too — so a
  disputed interest calculation from three years ago can be re-derived rather
  than argued about.

Above that sits the product layer: sandboxed WASM contracts that read an
account's own balances and emit postings. They cannot see the network, the
clock, or other accounts' state. That restriction is what keeps the fast path
parallelisable and the behaviour reproducible.

## 4. What this changes

**No batch window.** Accrued interest is computed as a pure function of the
balance timeline and the rate schedule, evaluated on read. Only capitalisation
writes postings, monthly rather than daily, and it can be spread across the
period. A nightly O(accounts) job becomes O(1) per query.

**Throughput one to two orders of magnitude higher.** The bottleneck in a
correctly designed ledger is durability round-trips, not CPU. Batching
thousands of postings per fsync puts six figures per second within reach on a
single node, without sharding.

**Backdating is ordinary.** Every posting carries both a value time and a
booking time. "Balance as of 30 June, as known on 15 July" is a normal query,
not a reconstruction project. History is never mutated; corrections are new
postings with old value dates.

**Product change measured in days.** A product is a content-addressed contract
version. Accounts record which version they run. Migration is an explicit,
logged event with an explicit conversion hook, so "which accounts are on the
old interest rule" is a query rather than an investigation.

**Agentic payments become expressible.** Autonomous agents transacting on a
principal's behalf need revocable, bounded, auditable spending authority. The
existing answer is virtual cards, which fake it: limits are enforced by the
issuer, revocation is eventually consistent, and the audit trail is a card
statement. Here the primitives already exist — two-phase holds for reserving
funds before committing, per-agent balance addresses under a single account,
and contract-enforced limits evaluated in the fast path. Authority is granted,
bounded, revoked and audited inside the ledger itself, and every agent action
is replayable.

## 5. Non-goals

Not a payment scheme, not a bank, not a general-purpose blockchain. No attempt
at multi-party consensus between distrusting institutions. Regulatory reporting
and screening are downstream consumers of the change stream, not core concerns.
Not seeking production deployment or commercial use.

## 6. Honest scope

The ledger core is a finishable project. A "core banking system" in the sense a
bank would recognise — products, statements, reconciliation, compliance,
operations tooling — is years of work. This project targets the core and one or
two products deep enough to prove the contract model, and treats the rest as
demonstrated rather than delivered.
