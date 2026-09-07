//! `M1` "done when": arbitrary command sequences hold per-denomination
//! zero-sum.
//!
//! This uses a small hand-rolled seeded PRNG rather than an external
//! property-testing crate. That is a deliberate fit for this project, not
//! a shortcut: the design explicitly calls for "seeded PRNG drives
//! scheduling... failures reproduce from a seed" (`ledger-sim`, a later
//! milestone) as the whole testing strategy, so this is a first, minimal
//! instance of exactly that idea rather than a dependency on someone
//! else's shrinking engine. Every seed here is printed on failure and can
//! be re-run in isolation by hand.

use ledger_core::{Command, State, apply};
use ledger_types::balance::{BalanceKey, Phase};
use ledger_types::ids::{AccountId, AddressId, ClientKey, DenomId};
use ledger_types::posting::{Batch, Posting, Side};
use ledger_types::time::{LogTime, ValueTime};
use std::collections::BTreeMap;

/// xorshift64* -- small, fast, and entirely deterministic given a seed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // Avoid the degenerate all-zero state.
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

const NUM_ACCOUNTS: u64 = 5;
const NUM_DENOMS: u16 = 2;
const OPS_PER_SEED: usize = 200;
const NUM_SEEDS: u64 = 300;

enum Op {
    Open(u64),
    Close(u64),
    Transfer {
        key: u128,
        from: u64,
        to: u64,
        denom: u16,
        amount: u128,
    },
}

fn random_op(rng: &mut Rng) -> Op {
    match rng.below(3) {
        0 => Op::Open(rng.below(NUM_ACCOUNTS)),
        1 => Op::Close(rng.below(NUM_ACCOUNTS)),
        _ => Op::Transfer {
            key: rng.next_u64() as u128,
            from: rng.below(NUM_ACCOUNTS),
            to: rng.below(NUM_ACCOUNTS),
            denom: rng.below(NUM_DENOMS as u64) as u16,
            amount: 1 + rng.below(999) as u128,
        },
    }
}

fn total_net_per_denom(state: &State) -> BTreeMap<u16, i128> {
    let mut totals: BTreeMap<u16, i128> = BTreeMap::new();
    for (bkey, balance) in state.balances.iter() {
        *totals.entry(bkey.denom.0).or_insert(0) += balance.net();
    }
    totals
}

fn run_seed(seed: u64) {
    let mut rng = Rng::new(seed);
    let mut state = State::new();

    for i in 0..OPS_PER_SEED {
        let at = LogTime(i as u64);
        let cmd = match random_op(&mut rng) {
            Op::Open(a) => Command::OpenAccount {
                account: AccountId(a),
            },
            Op::Close(a) => Command::CloseAccount {
                account: AccountId(a),
            },
            Op::Transfer {
                key,
                from,
                to,
                denom,
                amount,
            } => {
                if from == to {
                    continue;
                }
                Command::PostBatch(Batch {
                    idempotency_key: ClientKey(key),
                    value_time: ValueTime(0),
                    postings: vec![
                        Posting {
                            key: BalanceKey {
                                account: AccountId(from),
                                address: AddressId::DEFAULT,
                                denom: DenomId(denom),
                                phase: Phase::Committed,
                            },
                            side: Side::Debit,
                            amount,
                        },
                        Posting {
                            key: BalanceKey {
                                account: AccountId(to),
                                address: AddressId::DEFAULT,
                                denom: DenomId(denom),
                                phase: Phase::Committed,
                            },
                            side: Side::Credit,
                            amount,
                        },
                    ],
                })
            }
        };

        // Every outcome -- success or rejection -- must leave the
        // invariant intact. On rejection we simply don't advance `state`,
        // matching how a real caller keeps its last-known-good state on
        // error.
        if let Ok((next, _)) = apply(&state, &cmd, at) {
            state = next;
        }

        for (&denom, &net) in total_net_per_denom(&state).iter() {
            assert_eq!(
                net, 0,
                "seed {seed} step {i}: denom {denom} net is {net}, expected 0"
            );
        }
    }
}

#[test]
fn zero_sum_holds_per_denomination_across_many_seeds() {
    for seed in 0..NUM_SEEDS {
        run_seed(seed);
    }
}
