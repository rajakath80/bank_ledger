//! A scratch example for eyeballing what M2 actually does: apply a few
//! commands live, print the resulting log as JSON, replay it from scratch,
//! and confirm the two states match.
//!
//! Run with: cargo run -p ledger-log --example inspect_log

use ledger_core::{Command, State, apply};
use ledger_log::{Record, encode_log, replay};
use ledger_types::balance::{BalanceKey, Phase};
use ledger_types::ids::{AccountId, AddressId, ClientKey, DenomId};
use ledger_types::posting::{Batch, Posting, Side};
use ledger_types::time::{LogTime, ValueTime};

fn main() {
    let mut state = State::new();
    let mut records = Vec::new();

    let push = |state: &mut State, records: &mut Vec<Record>, at: u64, cmd: Command| {
        let (next, effects) = apply(state, &cmd, LogTime(at)).expect("command should be accepted");
        println!("applied at t={at}: {effects:?}");
        *state = next;
        records.push(Record::v1(LogTime(at), cmd));
    };

    push(
        &mut state,
        &mut records,
        0,
        Command::OpenAccount {
            account: AccountId(1),
        },
    );
    push(
        &mut state,
        &mut records,
        1,
        Command::OpenAccount {
            account: AccountId(2),
        },
    );
    push(
        &mut state,
        &mut records,
        2,
        Command::PostBatch(Batch {
            idempotency_key: ClientKey(1),
            value_time: ValueTime(0),
            postings: vec![
                Posting {
                    key: BalanceKey {
                        account: AccountId(1),
                        address: AddressId::DEFAULT,
                        denom: DenomId(1),
                        phase: Phase::Committed,
                    },
                    side: Side::Debit,
                    amount: 250,
                },
                Posting {
                    key: BalanceKey {
                        account: AccountId(2),
                        address: AddressId::DEFAULT,
                        denom: DenomId(1),
                        phase: Phase::Committed,
                    },
                    side: Side::Credit,
                    amount: 250,
                },
            ],
        }),
    );

    println!("\n--- log on the wire (newline-delimited JSON) ---");
    let encoded = encode_log(&records).unwrap();
    print!("{}", String::from_utf8_lossy(&encoded));

    println!("\n--- replaying from empty ---");
    let replayed = replay(&records).unwrap();

    println!(
        "live  balances: {:?}",
        state.balances.iter().collect::<Vec<_>>()
    );
    println!(
        "replay balances: {:?}",
        replayed.balances.iter().collect::<Vec<_>>()
    );
    assert_eq!(state, replayed);
    assert_eq!(state.canonical_bytes(), replayed.canonical_bytes());
    println!("\nreplay matches live application, byte for byte.");
}
