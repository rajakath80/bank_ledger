//! Ledger Guardian: an MCP (stdio) server exposing four read-only tools
//! over an existing `ledger-store` durable ledger --
//! `get_account_balance`, `get_journal_entries`, `run_reconciliation`,
//! `investigate_transaction`.
//!
//! This is deliberately a thin wrapper: every tool below is a few lines
//! calling straight into `ledger-core`/`ledger-log` functions that already
//! exist and are already tested (`State::balances_for_account`,
//! `State::total_net_per_denom`, `ledger_log::entries_for_account`,
//! `ledger_log::entry_for_idempotency_key`). Nothing here re-implements
//! ledger logic -- it only serializes the results as MCP tool responses.
//!
//! # Data freshness
//!
//! The ledger (state + full record history) is loaded once at startup via
//! `ledger_store::recover_read_only` + `ledger_log::replay`, then held in
//! memory for the life of the process. `recover_read_only` -- not
//! `SegmentedFileStorage::open` -- is deliberate: `open` unconditionally
//! opens the active segment file in append+write mode even though this
//! server only ever reads, which fails outright if the ledger directory
//! is mounted read-only (e.g. a Docker `:ro` bind mount). This is the
//! right amount of machinery for a POC pointed at a ledger nothing else
//! is concurrently writing to; a live read service watching a ledger
//! under active write traffic would need to re-open/re-replay (or
//! subscribe to a change stream -- `ledger-core`'s M5) instead of loading
//! once. Not built here because it isn't needed yet.
//!
//! # Run it
//!
//! ```text
//! cargo run -p ledger-guardian -- /path/to/ledger-store-dir
//! ```
//!
//! Register it with an MCP client (Claude Desktop, or your own driver) as
//! a stdio server pointed at that same binary + argument.

use ledger_core::{Command, State};
use ledger_log::{Record, decode_payload, entries_for_account, entry_for_idempotency_key, replay};
use ledger_store::recover_read_only;
use ledger_types::ids::{AccountId, ClientKey};
use rmcp::{
    ServerHandler, ServiceExt, handler::server::tool::ToolRouter,
    handler::server::wrapper::Parameters, model::*, tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Deserialize, JsonSchema)]
struct AccountIdParams {
    /// The account id to look up (the same integer used when the account
    /// was opened on the ledger).
    account_id: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IdempotencyKeyParams {
    /// The idempotency key of the batch to investigate.
    idempotency_key: u128,
}

/// Everything the server needs, loaded once at startup: the replayed
/// `State` (for balance/reconciliation queries) and the full decoded
/// record history (for journal/investigation queries that need more than
/// `State`'s bounded, evictable dedup index can answer).
///
/// `tool_router` is written by `#[tool_router]`'s generated code and
/// required by the macro machinery, but nothing in this file reads it back
/// directly -- dispatch happens through `#[tool_handler]`'s generated
/// `ServerHandler` methods, not a path the compiler's dead-code checker
/// can see from here. `#[allow(dead_code)]` rather than removing the
/// field, since removing it breaks what `#[tool_router]` expects to find
/// on the struct.
#[derive(Clone)]
struct LedgerGuardian {
    state: State,
    records: Arc<Vec<Record>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl LedgerGuardian {
    fn load(ledger_dir: &str) -> anyhow::Result<Self> {
        let outcome = recover_read_only(ledger_dir)?;
        if outcome.discarded_tail_bytes > 0 {
            eprintln!(
                "warning: {} trailing bytes discarded as torn/corrupt on load -- the ledger's last write may be incomplete",
                outcome.discarded_tail_bytes
            );
        }

        let records: Vec<Record> = outcome
            .records
            .iter()
            .map(|payload| decode_payload(payload))
            .collect::<Result<_, _>>()
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let state = replay(&records).map_err(|(i, rejection)| {
            anyhow::anyhow!("record {i} in the log was rejected on replay: {rejection:?}")
        })?;

        Ok(LedgerGuardian {
            state,
            records: Arc::new(records),
            tool_router: Self::tool_router(),
        })
    }

    #[tool(
        description = "Get every balance address held by an account (credit, debit, net, per denomination and phase)."
    )]
    fn get_account_balance(
        &self,
        Parameters(AccountIdParams { account_id }): Parameters<AccountIdParams>,
    ) -> String {
        let entries = self.state.balances_for_account(AccountId(account_id));
        let rows: Vec<serde_json::Value> = entries
            .into_iter()
            .map(|(key, balance)| {
                serde_json::json!({
                    "address": key.address.0,
                    "denom": key.denom.0,
                    "phase": format!("{:?}", key.phase),
                    "credit": balance.credit,
                    "debit": balance.debit,
                    "net": balance.net(),
                })
            })
            .collect();
        serde_json::to_string_pretty(&rows).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
    }

    #[tool(
        description = "Get every log entry (open, close, or posted batch) that touched a given account, in order."
    )]
    fn get_journal_entries(
        &self,
        Parameters(AccountIdParams { account_id }): Parameters<AccountIdParams>,
    ) -> String {
        let entries = entries_for_account(&self.records, AccountId(account_id));
        let rows: Vec<serde_json::Value> = entries
            .into_iter()
            .map(|record| {
                serde_json::json!({
                    "at": record.at().0,
                    "command": describe_command(record.command()),
                })
            })
            .collect();
        serde_json::to_string_pretty(&rows).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
    }

    #[tool(
        description = "Check whether the ledger balances: sums the net value of every open balance, per denomination, across the whole ledger. Should always be zero -- the state machine enforces this on every write, so a non-zero result here indicates a bug or tampering, not a normal reconciliation failure to fix."
    )]
    fn run_reconciliation(&self) -> String {
        let totals = self.state.total_net_per_denom();
        let all_zero = totals.values().all(|&net| net == 0);
        let rows: Vec<serde_json::Value> = totals
            .into_iter()
            .map(|(denom, net)| serde_json::json!({ "denom": denom.0, "net": net }))
            .collect();
        serde_json::to_string_pretty(
            &serde_json::json!({ "balanced": all_zero, "totals_per_denom": rows }),
        )
        .unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
    }

    #[tool(
        description = "Find the batch recorded under a specific idempotency key, and show exactly what it posted."
    )]
    fn investigate_transaction(
        &self,
        Parameters(IdempotencyKeyParams { idempotency_key }): Parameters<IdempotencyKeyParams>,
    ) -> String {
        match entry_for_idempotency_key(&self.records, ClientKey(idempotency_key)) {
            Some(record) => serde_json::to_string_pretty(&serde_json::json!({
                "found": true,
                "at": record.at().0,
                "command": describe_command(record.command()),
            }))
            .unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}")),
            None => serde_json::json!({ "found": false }).to_string(),
        }
    }
}

fn describe_command(command: &Command) -> serde_json::Value {
    match command {
        Command::OpenAccount { account } => {
            serde_json::json!({ "kind": "OpenAccount", "account": account.0 })
        }
        Command::CloseAccount { account } => {
            serde_json::json!({ "kind": "CloseAccount", "account": account.0 })
        }
        Command::Tick => serde_json::json!({ "kind": "Tick" }),
        Command::PostBatch(batch) => {
            let postings: Vec<serde_json::Value> = batch
                .postings
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "account": p.key.account.0,
                        "address": p.key.address.0,
                        "denom": p.key.denom.0,
                        "phase": format!("{:?}", p.key.phase),
                        "side": format!("{:?}", p.side),
                        "amount": p.amount,
                    })
                })
                .collect();
            serde_json::json!({
                "kind": "PostBatch",
                "idempotency_key": batch.idempotency_key.0,
                "value_time": batch.value_time.0,
                "postings": postings,
            })
        }
    }
}

#[tool_handler]
impl ServerHandler for LedgerGuardian {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`, so it can't be built with a
        // struct-literal `..Default::default()` outside its own crate --
        // start from the default value and assign into it instead.
        let mut info = ServerInfo::default();
        // `#[tool_handler]` implements `list_tools`/`call_tool` for us,
        // but it does NOT touch `get_info`'s declared capabilities --
        // without explicitly enabling `tools` here, a spec-following
        // client (the MCP Inspector's UI, notably) has no way to know
        // this server supports tools at all, and won't show a Tools tab
        // or ever call `tools/list`, even though the handlers work fine
        // if called directly.
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Read-only ledger forensics: get_account_balance, get_journal_entries, run_reconciliation, investigate_transaction. \
             This server never writes to the ledger."
                .into(),
        );
        info
    }
}

/// The one role this binary is ever allowed to serve. Read from an
/// environment variable set by whatever spawned this process, never from
/// anything an MCP client (or the model behind it) sends at call time --
/// the whole point is that the caller can't declare its own role. If
/// TechOps or FinOps's launcher is ever misconfigured to spawn this
/// binary instead of its own, this refuses to start rather than quietly
/// handing out ledger-forensics tools to the wrong agent.
const REQUIRED_ROLE: &str = "guardian";

fn check_role() -> anyhow::Result<()> {
    let role = std::env::var("LEDGER_AGENT_ROLE").unwrap_or_default();
    if role != REQUIRED_ROLE {
        anyhow::bail!(
            "ledger-guardian only serves the '{REQUIRED_ROLE}' role, but LEDGER_AGENT_ROLE={role:?}. \
             Refusing to start -- check which server your launcher meant to spawn."
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    check_role()?;

    let ledger_dir = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: ledger-guardian <path-to-ledger-store-dir>"))?;

    let guardian = LedgerGuardian::load(&ledger_dir)?;
    eprintln!(
        "ledger-guardian: loaded {} records from {ledger_dir}",
        guardian.records.len()
    );

    let service = guardian.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
