//! Seed a `SegmentedFileStorage` directory with a handful of accounts and
//! postings, and leave it on disk -- unlike `durable_ledger.rs`, which
//! cleans up after itself because it's a crash-recovery demo, not a data
//! source. Run this once, point `ledger-guardian` at the resulting
//! directory, and you have something real to query.
//!
//! Run with: cargo run -p ledger-store --example seed_ledger -- <dir>

use ledger_core::{Command, State, apply};
use ledger_log::{Record, RecordStorage, encode_payload};
use ledger_store::SegmentedFileStorage;
use ledger_types::balance::{BalanceKey, Phase};
use ledger_types::ids::{AccountId, AddressId, ClientKey, DenomId};
use ledger_types::posting::{Batch, Posting, Side};
use ledger_types::time::{LogTime, ValueTime};

fn main() -> anyhow::Result<()> {
    let dir = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: seed_ledger <dir>"))?;

    // Refuse to run against a directory that already has ledger data.
    // This example always starts from `State::new()` and appends a fixed
    // command sequence beginning at LogTime(0) -- running it twice against
    // the same directory appends a *second* OpenAccount(1) onto the real
    // durable log, which replay correctly rejects the moment anyone reads
    // the whole log back (`AccountAlreadyExists` at the colliding record).
    // Catching that here, with a clear message, beats discovering it later
    // as a cryptic replay failure in a different tool.
    if std::path::Path::new(&dir).join("header.json").exists() {
        anyhow::bail!(
            "{dir} already contains a ledger (header.json exists). This example always seeds from empty and \
             is not safe to run twice against the same directory -- remove it first (`rm -rf {dir}`) or pass a fresh path."
        );
    }

    // Structured JSON tracing, one line per span, written alongside the
    // ledger itself. This is what actually produces data for a future
    // TechOps `query_logs`/`get_latency_metrics` tool to read -- without a
    // subscriber like this installed somewhere, `ledger-core`'s and
    // `ledger-store`'s tracing calls are no-ops (see their doc comments).
    std::fs::create_dir_all(&dir)?;
    let trace_path = std::path::Path::new(&dir).join("trace.jsonl");
    let trace_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&trace_path)?;
    tracing_subscriber::fmt()
        .json()
        .with_writer(trace_file)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .init();
    println!(
        "writing structured trace events to {}",
        trace_path.display()
    );

    // Backdated across a real spread of days, not both left at the same
    // instant -- see the design doc's ValueTime(0) note: with every
    // posting sharing one timestamp, distinct_value_times-dependent
    // signals (ledger-credit's lookback window, in particular) have
    // nothing to work with. Ten days ago and three days ago gives a
    // credible income-like pattern out of the box, without waiting real
    // days for diversity to accumulate through live use.
    let ten_days_ago = ValueTime(ValueTime::today().0 - 10);
    let three_days_ago = ValueTime(ValueTime::today().0 - 3);

    let commands = [
        Command::OpenAccount {
            account: AccountId(1),
        },
        Command::OpenAccount {
            account: AccountId(2),
        },
        Command::OpenAccount {
            account: AccountId(3),
        },
        Command::PostBatch(Batch {
            idempotency_key: ClientKey(1),
            value_time: ten_days_ago,
            postings: vec![
                Posting {
                    key: BalanceKey {
                        account: AccountId(1),
                        address: AddressId::DEFAULT,
                        denom: DenomId(1),
                        phase: Phase::Committed,
                    },
                    side: Side::Debit,
                    amount: 10_000,
                },
                Posting {
                    key: BalanceKey {
                        account: AccountId(2),
                        address: AddressId::DEFAULT,
                        denom: DenomId(1),
                        phase: Phase::Committed,
                    },
                    side: Side::Credit,
                    amount: 10_000,
                },
            ],
        }),
        Command::PostBatch(Batch {
            idempotency_key: ClientKey(2),
            value_time: three_days_ago,
            postings: vec![
                Posting {
                    key: BalanceKey {
                        account: AccountId(2),
                        address: AddressId::DEFAULT,
                        denom: DenomId(1),
                        phase: Phase::Committed,
                    },
                    side: Side::Debit,
                    amount: 2_500,
                },
                Posting {
                    key: BalanceKey {
                        account: AccountId(3),
                        address: AddressId::DEFAULT,
                        denom: DenomId(1),
                        phase: Phase::Committed,
                    },
                    side: Side::Credit,
                    amount: 2_500,
                },
            ],
        }),
    ];

    let mut state = State::new();
    let mut store = SegmentedFileStorage::open(&dir)?;
    let total_commands = commands.len();

    for (i, cmd) in commands.into_iter().enumerate() {
        let at = LogTime(i as u64);
        let (next, effects) = apply(&state, &cmd, at).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        state = next;
        println!("applied at t={i}: {effects:?}");

        let record = Record::v1(at, cmd);
        let payload = encode_payload(&record).map_err(|e| anyhow::anyhow!("{}", e.0))?;
        store.append(&payload)?;
    }
    store.sync()?;

    println!("\nseeded {dir} with {total_commands} commands.");
    println!(
        "final balances: {:?}",
        state.balances.iter().collect::<Vec<_>>()
    );
    println!("\nnow run: cargo run -p ledger-guardian -- {dir}");
    Ok(())
}
