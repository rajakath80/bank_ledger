//! End-to-end M3 demonstration: real `ledger_core::Command`s written
//! through `SegmentedFileStorage`, a simulated crash that tears the last
//! write, and recovery feeding straight back into `ledger_core::apply` to
//! rebuild the exact same `State` minus only the torn command.
//!
//! Run with: cargo run -p ledger-store --example durable_ledger

use ledger_core::{Command, State, apply};
use ledger_log::{Record, RecordStorage, decode_payload, encode_payload};
use ledger_store::SegmentedFileStorage;
use ledger_types::balance::{BalanceKey, Phase};
use ledger_types::ids::{AccountId, AddressId, ClientKey, DenomId};
use ledger_types::posting::{Batch, Posting, Side};
use ledger_types::time::{LogTime, ValueTime};
use std::fs;

fn main() {
    let dir = std::env::temp_dir().join("ledger_store_example_durable_ledger");
    let _ = fs::remove_dir_all(&dir); // start clean each run

    let commands = [
        Command::OpenAccount {
            account: AccountId(1),
        },
        Command::OpenAccount {
            account: AccountId(2),
        },
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
                    amount: 500,
                },
                Posting {
                    key: BalanceKey {
                        account: AccountId(2),
                        address: AddressId::DEFAULT,
                        denom: DenomId(1),
                        phase: Phase::Committed,
                    },
                    side: Side::Credit,
                    amount: 500,
                },
            ],
        }),
        // This one will get torn by the simulated crash below.
        Command::PostBatch(Batch {
            idempotency_key: ClientKey(2),
            value_time: ValueTime(0),
            postings: vec![
                Posting {
                    key: BalanceKey {
                        account: AccountId(2),
                        address: AddressId::DEFAULT,
                        denom: DenomId(1),
                        phase: Phase::Committed,
                    },
                    side: Side::Debit,
                    amount: 100,
                },
                Posting {
                    key: BalanceKey {
                        account: AccountId(1),
                        address: AddressId::DEFAULT,
                        denom: DenomId(1),
                        phase: Phase::Committed,
                    },
                    side: Side::Credit,
                    amount: 100,
                },
            ],
        }),
    ];

    // --- "Before the crash": apply and durably write every command. ---
    let mut state = State::new();
    {
        let mut store = SegmentedFileStorage::open(&dir).unwrap();
        for (i, cmd) in commands.iter().enumerate() {
            let at = LogTime(i as u64);
            let (next, effects) = apply(&state, cmd, at).unwrap();
            state = next;
            println!("applied {cmd:?} -> {effects:?}");

            let record = Record::v1(at, cmd.clone());
            let payload = encode_payload(&record).unwrap();
            store.append(&payload).unwrap();
            store.sync().unwrap();
        }
    }
    println!(
        "\nstate before crash: {:?}\n",
        state.balances.iter().collect::<Vec<_>>()
    );

    // --- Simulate a crash that tears the last write in half. ---
    let segment = fs::read_dir(dir.join("segments"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let full_len = fs::metadata(&segment).unwrap().len();
    let file = fs::OpenOptions::new().write(true).open(&segment).unwrap();
    file.set_len(full_len - 8).unwrap(); // chop off the tail of the last frame
    drop(file);
    println!(
        "simulated a crash: truncated the last {} bytes of the active segment\n",
        8
    );

    // --- Recovery: read back only what's intact, replay it. ---
    let store = SegmentedFileStorage::open(&dir).unwrap();
    let outcome = store.recover().unwrap();
    println!(
        "recovered {} of {} records ({} bytes discarded as torn)",
        outcome.records.len(),
        commands.len(),
        outcome.discarded_tail_bytes
    );

    let mut recovered_state = State::new();
    for payload in &outcome.records {
        let record = decode_payload(payload).unwrap();
        let (next, _) = apply(&recovered_state, record.command(), record.at()).unwrap();
        recovered_state = next;
    }

    println!(
        "state after recovery: {:?}\n",
        recovered_state.balances.iter().collect::<Vec<_>>()
    );

    assert_eq!(
        outcome.records.len(),
        commands.len() - 1,
        "the last (torn) command should not have recovered"
    );
    println!(
        "recovery correctly dropped exactly the one command that was mid-write when the crash happened."
    );

    let _ = fs::remove_dir_all(&dir);
}
