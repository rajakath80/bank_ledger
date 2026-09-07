//! Baseline throughput for `apply` on a single balanced `PostBatch`.
//!
//! `M1` "done when": a criterion bench gives a baseline number. This
//! sandbox's toolchain (`rustc 1.75`, installed via `apt` because network
//! access is restricted to a fixed domain allowlist that doesn't include
//! `static.rust-lang.org`/`rustup`) can't build recent `criterion`: its
//! transitive `toml`/`serde_spanned` dependency now requires Cargo's
//! `edition2024` feature, unstable before Cargo 1.85. Rather than pin a
//! chain of transitive versions by hand, this is a plain `std::time`
//! measurement with the same shape criterion would report. Swap in real
//! `criterion` (see the commented `[dev-dependencies]`/`[[bench]]` note in
//! `Cargo.toml`) once building on a newer toolchain.
//!
//! Run with `cargo run --release --example bench_apply`.

use std::time::Instant;

use ledger_core::{Command, State, apply};
use ledger_types::balance::{BalanceKey, Phase};
use ledger_types::ids::{AccountId, AddressId, ClientKey, DenomId};
use ledger_types::posting::{Batch, Posting, Side};
use ledger_types::time::{LogTime, ValueTime};

fn setup() -> State {
    let mut state = State::new();
    for i in 0..2u64 {
        let (next, _) = apply(
            &state,
            &Command::OpenAccount {
                account: AccountId(i),
            },
            LogTime(i),
        )
        .unwrap();
        state = next;
    }
    state
}

fn main() {
    let base = setup();
    const ITERATIONS: u128 = 200_000;

    let start = Instant::now();
    for counter in 0..ITERATIONS {
        let cmd = Command::PostBatch(Batch {
            idempotency_key: ClientKey(counter),
            value_time: ValueTime(0),
            postings: vec![
                Posting {
                    key: BalanceKey {
                        account: AccountId(0),
                        address: AddressId::DEFAULT,
                        denom: DenomId(1),
                        phase: Phase::Committed,
                    },
                    side: Side::Debit,
                    amount: 1,
                },
                Posting {
                    key: BalanceKey {
                        account: AccountId(1),
                        address: AddressId::DEFAULT,
                        denom: DenomId(1),
                        phase: Phase::Committed,
                    },
                    side: Side::Credit,
                    amount: 1,
                },
            ],
        });
        apply(&base, &cmd, LogTime(1)).unwrap();
    }
    let elapsed = start.elapsed();

    let per_op_ns = elapsed.as_nanos() as f64 / ITERATIONS as f64;
    let ops_per_sec = 1_000_000_000.0 / per_op_ns;

    println!("apply single balanced two-leg batch:");
    println!("  {ITERATIONS} iterations in {elapsed:?}");
    println!(
        "  {per_op_ns:.1} ns/op  (~{ops_per_sec:.0} batches/sec, single-threaded, debug-affected unless --release)"
    );
}
