//! TechOps: an MCP (stdio) server exposing four tools --
//! `query_logs`, `get_latency_metrics`, `get_trace`, `create_incident`.
//!
//! Deliberately has **no dependency on `ledger-core` or `ledger-types`**.
//! TechOps never touches ledger domain concepts (accounts, postings,
//! balances) at all -- it only reads the structured `tracing` output that
//! `ledger-core`/`ledger-store` emit (see their instrumented `apply`,
//! `append`, `sync`, `recover`) and writes plain JSON incident records
//! through the same generic `RecordStorage` trait `ledger-store` uses for
//! the ledger itself. That's a real boundary, not an accident: it's what
//! makes TechOps's RBAC role meaningful -- it structurally *cannot* read
//! or write a balance or a posting, because the types to do so were never
//! imported.
//!
//! `create_incident` is this server's one write path, and it writes only
//! to its own separate incident log (a distinct `SegmentedFileStorage`
//! directory), never to the ledger's own segments.
//!
//! # Run it
//!
//! ```text
//! LEDGER_AGENT_ROLE=techops cargo run -p ledger-techops -- \
//!     /tmp/demo-ledger /tmp/demo-ledger-incidents
//! ```
//!
//! The first argument is the ledger directory to watch for `trace.jsonl`
//! (written by `ledger-store`'s `seed_ledger` example, or any future
//! write-path binary that installs the same subscriber); the second is
//! where this server's own incident log lives.

use ledger_log::RecordStorage;
use ledger_store::SegmentedFileStorage;
use rmcp::{
    ServerHandler, ServiceExt, handler::server::tool::ToolRouter,
    handler::server::wrapper::Parameters, model::*, tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, JsonSchema)]
struct QueryLogsParams {
    /// Optional substring to filter trace lines by (case-insensitive,
    /// matched against the whole JSON line). Omit to return the most
    /// recent entries unfiltered.
    keyword: Option<String>,
    /// Maximum number of matching lines to return, most recent first.
    /// Defaults to 20.
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetLatencyMetricsParams {
    /// Optional operation kind to filter to -- either a command kind
    /// ("PostBatch", "OpenAccount", "CloseAccount", "Tick") or a storage
    /// operation name ("append", "sync", "recover"). Omit for metrics
    /// across every kind, grouped separately.
    kind: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetTraceParams {
    /// Optional exact log_time to filter to. Only meaningful for
    /// ledger-core `apply` spans, which are the only ones that carry it.
    log_time: Option<u64>,
    /// Optional substring to filter by tracing target, e.g.
    /// "ledger_core::apply" or "ledger_store".
    target: Option<String>,
    /// Maximum number of matching entries to return. Defaults to 20.
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CreateIncidentParams {
    /// Short incident title.
    title: String,
    /// Full description of what was observed.
    description: String,
    /// One of "low", "medium", "high", "critical".
    severity: String,
}

/// Parse a human-readable duration string (as `tracing-subscriber`'s JSON
/// formatter writes them, e.g. "18.8µs", "1.20ms", "500ns", "2.3s") into
/// nanoseconds. Splits at the first non-digit, non-'.' character rather
/// than matching fixed byte-length suffixes, so it doesn't matter whether
/// the micro sign is `µ` (U+00B5) or written as `us`.
fn parse_duration_ns(s: &str) -> Option<f64> {
    let split_at = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (num_part, unit) = s.split_at(split_at);
    let value: f64 = num_part.parse().ok()?;
    let multiplier = match unit.trim() {
        "ns" => 1.0,
        "µs" | "us" => 1_000.0,
        "ms" => 1_000_000.0,
        "s" => 1_000_000_000.0,
        _ => return None,
    };
    Some(value * multiplier)
}

/// The value at percentile `p` (0.0..=1.0) of an already-sorted slice.
fn percentile(sorted_values: &[f64], p: f64) -> f64 {
    if sorted_values.is_empty() {
        return 0.0;
    }
    let idx = (((sorted_values.len() - 1) as f64) * p).round() as usize;
    sorted_values[idx.min(sorted_values.len() - 1)]
}

#[derive(Clone)]
struct TechOps {
    trace_path: PathBuf,
    incidents_dir: String,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl TechOps {
    fn new(ledger_dir: &str, incidents_dir: &str) -> Self {
        let trace_path = PathBuf::from(ledger_dir).join("trace.jsonl");
        TechOps {
            trace_path,
            incidents_dir: incidents_dir.to_string(),
            tool_router: Self::tool_router(),
        }
    }

    /// Every parseable line in the trace file, oldest first. Missing file
    /// (nothing has written a trace yet) or a malformed line (e.g. a
    /// torn write if the process died mid-flush) is treated as "no data
    /// there" rather than an error -- a read tool should degrade
    /// gracefully, not fail the whole call over one bad line.
    fn read_trace_events(&self) -> Vec<serde_json::Value> {
        let Ok(content) = std::fs::read_to_string(&self.trace_path) else {
            return Vec::new();
        };
        content
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    #[tool(
        description = "Search trace log lines for a keyword, most recent first. Omit the keyword to just see recent activity."
    )]
    fn query_logs(
        &self,
        Parameters(QueryLogsParams { keyword, limit }): Parameters<QueryLogsParams>,
    ) -> String {
        let limit = limit.unwrap_or(20);
        let events = self.read_trace_events();
        let needle = keyword.map(|k| k.to_lowercase());

        let matches: Vec<&serde_json::Value> = events
            .iter()
            .rev()
            .filter(|event| match &needle {
                Some(kw) => event.to_string().to_lowercase().contains(kw.as_str()),
                None => true,
            })
            .take(limit)
            .collect();

        serde_json::to_string_pretty(&matches).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
    }

    #[tool(
        description = "Aggregate operation latency from the trace log: count, min, max, average, p50 and p95, all in nanoseconds, grouped by operation kind. Optionally filter to one kind."
    )]
    fn get_latency_metrics(
        &self,
        Parameters(GetLatencyMetricsParams { kind }): Parameters<GetLatencyMetricsParams>,
    ) -> String {
        let events = self.read_trace_events();
        let mut by_group: std::collections::BTreeMap<String, Vec<f64>> =
            std::collections::BTreeMap::new();

        for event in &events {
            let Some(busy_str) = event["fields"]["time.busy"].as_str() else {
                continue;
            };
            let Some(ns) = parse_duration_ns(busy_str) else {
                continue;
            };
            let group = event["span"]["kind"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| {
                    event["span"]["name"]
                        .as_str()
                        .unwrap_or("unknown")
                        .to_string()
                });
            if let Some(filter) = &kind {
                if &group != filter {
                    continue;
                }
            }
            by_group.entry(group).or_default().push(ns);
        }

        let rows: Vec<serde_json::Value> = by_group
            .into_iter()
            .map(|(group, mut values)| {
                values.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let count = values.len();
                let avg = values.iter().sum::<f64>() / count as f64;
                serde_json::json!({
                    "kind": group,
                    "count": count,
                    "min_ns": values.first().copied().unwrap_or(0.0),
                    "max_ns": values.last().copied().unwrap_or(0.0),
                    "avg_ns": avg,
                    "p50_ns": percentile(&values, 0.50),
                    "p95_ns": percentile(&values, 0.95),
                })
            })
            .collect();

        serde_json::to_string_pretty(&rows).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
    }

    #[tool(
        description = "Get raw trace entries, optionally filtered by exact log_time and/or a substring of the tracing target. Most recent first."
    )]
    fn get_trace(
        &self,
        Parameters(GetTraceParams {
            log_time,
            target,
            limit,
        }): Parameters<GetTraceParams>,
    ) -> String {
        let limit = limit.unwrap_or(20);
        let events = self.read_trace_events();

        let matches: Vec<&serde_json::Value> = events
            .iter()
            .rev()
            .filter(|event| {
                let log_time_ok =
                    log_time.map_or(true, |lt| event["span"]["log_time"].as_u64() == Some(lt));
                let target_ok = target.as_ref().map_or(true, |t| {
                    event["target"]
                        .as_str()
                        .is_some_and(|actual| actual.contains(t.as_str()))
                });
                log_time_ok && target_ok
            })
            .take(limit)
            .collect();

        serde_json::to_string_pretty(&matches).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
    }

    #[tool(
        description = "File a new incident. Writes to TechOps's own append-only incident log -- a separate store from the ledger itself -- via the same durable, checksummed storage the ledger uses. Never touches the ledger's own segments."
    )]
    fn create_incident(
        &self,
        Parameters(CreateIncidentParams {
            title,
            description,
            severity,
        }): Parameters<CreateIncidentParams>,
    ) -> String {
        let filed_at_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let incident = serde_json::json!({
            "title": title,
            "description": description,
            "severity": severity,
            "filed_at_unix_ms": filed_at_unix_ms,
        });

        let outcome = (|| -> anyhow::Result<()> {
            let payload = serde_json::to_vec(&incident)?;
            let mut store = SegmentedFileStorage::open(&self.incidents_dir)?;
            store.append(&payload)?;
            store.sync()?;
            Ok(())
        })();

        match outcome {
            Ok(()) => {
                serde_json::json!({"filed": true, "title": title, "severity": severity}).to_string()
            }
            Err(e) => format!("{{\"error\": \"failed to file incident: {e}\"}}"),
        }
    }
}

#[tool_handler]
impl ServerHandler for TechOps {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]`: build the default, then
        // assign into it, rather than a struct literal.
        let mut info = ServerInfo::default();
        // Without this, `#[tool_handler]`'s `list_tools`/`call_tool` work
        // if called directly, but a client that checks declared
        // capabilities first (the MCP Inspector's UI) never finds out
        // tools exist at all -- see the identical note in
        // ledger-guardian.
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "TechOps: query_logs, get_latency_metrics, get_trace (all read-only, over ledger-core/ledger-store's trace \
             output), and create_incident (writes only to this server's own incident log, never to the ledger)."
                .into(),
        );
        info
    }
}

/// The one role this binary is ever allowed to serve. See
/// `ledger-guardian`'s identical check for the rationale.
const REQUIRED_ROLE: &str = "techops";

fn check_role() -> anyhow::Result<()> {
    let role = std::env::var("LEDGER_AGENT_ROLE").unwrap_or_default();
    if role != REQUIRED_ROLE {
        anyhow::bail!(
            "ledger-techops only serves the '{REQUIRED_ROLE}' role, but LEDGER_AGENT_ROLE={role:?}. \
             Refusing to start -- check which server your launcher meant to spawn."
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    check_role()?;

    let mut args = std::env::args().skip(1);
    let ledger_dir = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: ledger-techops <ledger-dir> <incidents-dir>"))?;
    let incidents_dir = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: ledger-techops <ledger-dir> <incidents-dir>"))?;

    let techops = TechOps::new(&ledger_dir, &incidents_dir);
    eprintln!(
        "ledger-techops: watching {} for traces, filing incidents to {incidents_dir}",
        techops.trace_path.display()
    );

    let service = techops.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
