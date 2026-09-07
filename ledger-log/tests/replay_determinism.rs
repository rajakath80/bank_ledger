//! `M2` "done when": run N random commands through `apply`; replay the
//! resulting log from empty; assert the two states are byte-identical.
//! This is the defining test from the design -- it is the single piece of
//! evidence that justifies the log/state-machine split -- and per the
//! project's own testing philosophy it is driven by a seeded PRNG rather
//! than an external property-testing crate (see
//! `ledger-core/tests/property_zero_sum.rs` for the rationale, which
//! applies equally here).
//! cargo run -p ledger-log --example inspect_log

use ledger_core::{Command, State, apply};
use ledger_log::{Record, decode_log, encode_log, replay};
use ledger_types::balance::{BalanceKey, Phase};
use ledger_types::ids::{AccountId, AddressId, ClientKey, DenomId};
use ledger_types::posting::{Batch, Posting, Side};
use ledger_types::time::{LogTime, ValueTime};

/// xorshift64* -- small, fast, deterministic given a seed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }
}

const NUM_ACCOUNTS: u64 = 4;
const OPS_PER_SEED: usize = 150;
const NUM_SEEDS: u64 = 300;

enum Op {
    Open(u64),
    Close(u64),
    Transfer {
        key: u128,
        from: u64,
        to: u64,
        amount: u128,
    },
    Tick,
}

fn random_op(rng: &mut Rng) -> Op {
    match rng.below(4) {
        0 => Op::Open(rng.below(NUM_ACCOUNTS)),
        1 => Op::Close(rng.below(NUM_ACCOUNTS)),
        2 => Op::Tick,
        _ => Op::Transfer {
            key: rng.next_u64() as u128,
            from: rng.below(NUM_ACCOUNTS),
            to: rng.below(NUM_ACCOUNTS),
            amount: 1 + rng.below(999) as u128,
        },
    }
}

fn to_command(op: Op) -> Option<Command> {
    match op {
        Op::Open(a) => Some(Command::OpenAccount {
            account: AccountId(a),
        }),
        Op::Close(a) => Some(Command::CloseAccount {
            account: AccountId(a),
        }),
        Op::Tick => Some(Command::Tick),
        Op::Transfer {
            key,
            from,
            to,
            amount,
        } => {
            if from == to {
                return None;
            }
            Some(Command::PostBatch(Batch {
                idempotency_key: ClientKey(key),
                value_time: ValueTime(0),
                postings: vec![
                    Posting {
                        key: BalanceKey {
                            account: AccountId(from),
                            address: AddressId::DEFAULT,
                            denom: DenomId(1),
                            phase: Phase::Committed,
                        },
                        side: Side::Debit,
                        amount,
                    },
                    Posting {
                        key: BalanceKey {
                            account: AccountId(to),
                            address: AddressId::DEFAULT,
                            denom: DenomId(1),
                            phase: Phase::Committed,
                        },
                        side: Side::Credit,
                        amount,
                    },
                ],
            }))
        }
    }
}

fn run_seed(seed: u64) {
    let mut rng = Rng::new(seed);
    let mut state = State::new();
    let mut records = Vec::new();

    for i in 0..OPS_PER_SEED {
        let Some(cmd) = to_command(random_op(&mut rng)) else {
            continue;
        };
        let at = LogTime(i as u64);
        // Only commands actually accepted when first written go in the
        // log, matching how a real client only sees success acknowledged
        // and durably recorded.
        if let Ok((next, _)) = apply(&state, &cmd, at) {
            state = next;
            records.push(Record::v1(at, cmd));
        }
    }

    // The log itself round-trips through its wire encoding...
    let encoded =
        encode_log(&records).unwrap_or_else(|e| panic!("seed {seed}: encode failed: {e}"));
    let decoded =
        decode_log(&encoded).unwrap_or_else(|e| panic!("seed {seed}: decode failed: {e}"));
    assert_eq!(
        records, decoded,
        "seed {seed}: log did not round-trip through its wire encoding"
    );

    // ...and replaying it reproduces the live-applied state.
    let replayed = replay(&decoded).unwrap_or_else(|e| {
        panic!("seed {seed}: replay rejected a record it should have accepted: {e:?}")
    });
    assert_eq!(
        state, replayed,
        "seed {seed}: replayed state diverged from live-applied state"
    );

    // Belt and braces: byte-identical canonical serialisation, not just
    // structural equality.
    let live_bytes = state.canonical_bytes();
    let replayed_bytes = replayed.canonical_bytes();
    assert_eq!(
        live_bytes, replayed_bytes,
        "seed {seed}: serialised bytes diverged even though states compared equal"
    );
}

#[test]
fn replay_is_byte_identical_to_live_application_across_many_seeds() {
    for seed in 0..NUM_SEEDS {
        run_seed(seed);
    }
}
