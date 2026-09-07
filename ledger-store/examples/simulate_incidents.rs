//! Inject realistic anomalies into an already-seeded ledger's trace log,
//! so TechOps's tools have real problems to find instead of only clean,
//! fast, successful operations.
//!
//! This does three things, each producing a genuine trace event via the
//! same instrumentation `apply`/`append`/`sync` already emit -- nothing
//! here is faked data bolted on afterward:
//!
//! 1. A real *rejected* command (`UnbalancedBatch`) against the actual
//!    loaded state -- `apply`'s own tracing call fires
//!    `outcome="rejected"` exactly as it would for a genuine bug.
//! 2. A second rejection (`AccountNotFound`), so there's more than one
//!    kind of failure to distinguish.
//! 3. A manually-held span shaped identically to a real `apply` span
//!    (`name="apply"`, `kind`, `log_time` fields) but deliberately slow --
//!    simulating a genuine performance regression without needing to
//!    actually make the state machine slow.
//!
//! Crucially, none of the rejected commands are written to the ledger's
//! segments -- exactly like a real client, only commands that were
//! actually accepted get appended. The ledger itself is untouched; only
//! `trace.jsonl` grows.
//!
//! Run with: cargo run -p ledger-store --example simulate_incidents -- <dir>
//! (the same directory you already ran `seed_ledger` against)

use ledger_core::{Command, apply};
use ledger_log::{RecordStorage, decode_payload};
use ledger_store::SegmentedFileStorage;
use ledger_types::balance::{BalanceKey, Phase};
use ledger_types::ids::{AccountId, AddressId, ClientKey, DenomId};
use ledger_types::posting::{Batch, Posting, Side};
use ledger_types::time::{LogTime, ValueTime};
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let dir = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: simulate_incidents <dir>"))?;

    // Append to the SAME trace file seed_ledger wrote, so TechOps sees one
    // continuous history rather than two disconnected files.
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

    // Load the real current state, so the rejections below are genuine
    // (referencing real accounts, real balances) rather than contrived.
    let store = SegmentedFileStorage::open(&dir)?;
    let outcome = store.recover()?;
    let records: Vec<_> = outcome
        .records
        .iter()
        .map(|p| decode_payload(p))
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let state = ledger_log::replay(&records)
        .map_err(|(i, r)| anyhow::anyhow!("record {i} rejected on replay: {r:?}"))?;

    println!(
        "loaded state with {} accounts",
        state.accounts.iter().count()
    );

    // --- Anomaly 1: a real UnbalancedBatch rejection. ---
    let unbalanced = Command::PostBatch(Batch {
        idempotency_key: ClientKey(9001),
        value_time: ValueTime(0),
        postings: vec![Posting {
            key: BalanceKey {
                account: AccountId(1),
                address: AddressId::DEFAULT,
                denom: DenomId(1),
                phase: Phase::Committed,
            },
            side: Side::Debit,
            amount: 50,
        }],
    });
    match apply(&state, &unbalanced, LogTime(9001)) {
        Ok(_) => println!("unexpected: unbalanced batch was accepted"),
        Err(rejection) => {
            println!("simulated rejection recorded: {rejection:?} (UnbalancedBatch, as intended)")
        }
    }

    // --- Anomaly 2: a real AccountNotFound rejection. ---
    let unknown_account = Command::PostBatch(Batch {
        idempotency_key: ClientKey(9002),
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
                amount: 10,
            },
            Posting {
                key: BalanceKey {
                    account: AccountId(999),
                    address: AddressId::DEFAULT,
                    denom: DenomId(1),
                    phase: Phase::Committed,
                },
                side: Side::Credit,
                amount: 10,
            },
        ],
    });
    match apply(&state, &unknown_account, LogTime(9002)) {
        Ok(_) => println!("unexpected: unknown-account batch was accepted"),
        Err(rejection) => {
            println!("simulated rejection recorded: {rejection:?} (AccountNotFound, as intended)")
        }
    }

    // --- Anomaly 3: a simulated slow operation. ---
    // Shaped exactly like a real `apply` span (same span name and field
    // names `kind`/`log_time`) so `get_latency_metrics` groups and reports
    // it the same way it would a genuine regression -- tagged with an
    // obviously-synthetic kind so nobody mistakes it for a real command.
    {
        let _span =
            tracing::info_span!("apply", kind = "SimulatedSlowBatch", log_time = 9003u64).entered();
        std::thread::sleep(Duration::from_millis(250));
    }
    println!("simulated a 250ms slow operation under kind=SimulatedSlowBatch");

    println!(
        "\ndone -- {} now has real rejections and one slow outlier for TechOps to find.",
        trace_path.display()
    );
    println!(
        "try in TechOps: query_logs({{\"keyword\":\"rejected\"}}), get_latency_metrics({{}}), get_trace({{\"log_time\":9003}})"
    );
    Ok(())
}
