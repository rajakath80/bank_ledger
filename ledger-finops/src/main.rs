//! FinOps: an MCP (stdio) server exposing four tools --
//! `get_product_volume`, `compare_periods` (both real, from the ledger's
//! own history via `ledger-tags`), and `get_infrastructure_cost`,
//! `calculate_unit_cost` (both synthetic estimates from live `docker
//! stats`, clearly labeled as such).
//!
//! # Honesty split, on purpose
//!
//! There is no real cloud bill on a local machine, so this server never
//! pretends to have one. `get_product_volume`/`compare_periods` read the
//! ledger's actual record history (via `ledger_store::recover_read_only`,
//! same read-only-safe path `ledger-guardian` uses) and `ledger-tags`'s
//! account-to-product map -- every number they return is real.
//! `get_infrastructure_cost`/`calculate_unit_cost` shell out to `docker
//! stats` for real CPU/memory usage, then multiply by an assumed unit
//! price to produce a dollar figure -- the *usage* is real, the *price*
//! is invented, and every response says so explicitly (`"synthetic":
//! true`) rather than letting a plausible-looking number pass as fact.
//!
//! Also worth knowing: `docker stats --no-stream` is a live snapshot, not
//! a historical record. There is no continuously-running sampler here, so
//! `get_infrastructure_cost` can only report "cost right now,
//! extrapolated," never "actual cost over the requested period." That
//! limitation is why `compare_periods` compares real volume, not cost --
//! comparing a repeatedly re-measured live snapshot across two arbitrary
//! historical windows would silently misrepresent it as historical data.
//!
//! # Requires Docker socket access
//!
//! Querying *other* containers' stats requires this server to reach the
//! host's Docker daemon, which means mounting `/var/run/docker.sock` into
//! its own container. That is effectively root-equivalent access to the
//! host -- anything with the socket can control every container and
//! image on the machine. Standard practice for tooling like this, but not
//! something to do without knowing the cost; see the Dockerfile for where
//! this gets wired up.
//!
//! # Run it
//!
//! ```text
//! LEDGER_AGENT_ROLE=finops cargo run -p ledger-finops -- \
//!     /tmp/demo-ledger /path/to/tags.json
//! ```

use ledger_core::Command;
use ledger_log::{Record, decode_payload};
use ledger_store::recover_read_only;
use ledger_tags::ProductTags;
use rmcp::{
    ServerHandler, ServiceExt, handler::server::tool::ToolRouter,
    handler::server::wrapper::Parameters, model::*, tool, tool_handler, tool_router,
    transport::stdio,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

/// An assumed on-demand price, roughly in the ballpark of common cloud
/// list prices -- not tied to any real contract or invoice. Every
/// response that uses this says so explicitly.
const ASSUMED_VCPU_HOURLY_USD: f64 = 0.04;
const ASSUMED_GB_HOURLY_USD: f64 = 0.005;

#[derive(Debug, Deserialize, JsonSchema)]
struct GetProductVolumeParams {
    /// Product name as it appears in the tags file, e.g. "savings".
    product: String,
    /// Optional inclusive value_time range to filter to. Omit either or
    /// both for all-time volume.
    start_value_time: Option<i64>,
    end_value_time: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetInfrastructureCostParams {
    /// Container names to measure (e.g. ["ledger-guardian",
    /// "ledger-techops"]). Omit to measure every container currently
    /// visible through the mounted Docker socket.
    containers: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CalculateUnitCostParams {
    /// Product name as it appears in the tags file.
    product: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ComparePeriodsParams {
    /// Product name as it appears in the tags file.
    product: String,
    period_a_start: i64,
    period_a_end: i64,
    period_b_start: i64,
    period_b_end: i64,
}

/// Parse a docker-stats percentage string like "0.15%" into a plain
/// number.
fn parse_percent(s: &str) -> Option<f64> {
    s.trim().trim_end_matches('%').parse().ok()
}

/// Parse one side of a docker-stats byte-size string (e.g. "15.5MiB",
/// "512KiB", "1.2GB") into bytes. Handles both binary (Ki/Mi/Gi) and
/// decimal (k/M/G) prefixes, since different Docker versions have used
/// both.
fn parse_mem_bytes(s: &str) -> Option<f64> {
    let s = s.trim();
    let split_at = s.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (num, unit) = s.split_at(split_at);
    let value: f64 = num.parse().ok()?;
    let multiplier = match unit.trim() {
        "B" => 1.0,
        "kB" | "KB" => 1_000.0,
        "KiB" => 1024.0,
        "MB" => 1_000_000.0,
        "MiB" => 1024.0 * 1024.0,
        "GB" => 1_000_000_000.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some(value * multiplier)
}

/// docker stats' `MemUsage` field looks like "15.5MiB / 1.943GiB" (used /
/// limit) -- this extracts just the used side.
fn parse_mem_usage_used(mem_usage: &str) -> Option<f64> {
    let used_part = mem_usage.split('/').next()?;
    parse_mem_bytes(used_part)
}

/// Run `docker stats --no-stream --format '{{json .}}'` for the given
/// container names (or every visible container if empty) and parse each
/// line as its own JSON object -- that flag produces newline-delimited
/// JSON, one object per container, not a JSON array.
fn run_docker_stats(containers: &[String]) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut cmd = std::process::Command::new("docker");
    cmd.arg("stats")
        .arg("--no-stream")
        .arg("--format")
        .arg("{{json .}}");
    for name in containers {
        cmd.arg(name);
    }
    let output = cmd.output()?;
    if !output.status.success() {
        anyhow::bail!(
            "docker stats exited with an error: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// Convert a set of docker-stats rows into a per-container cost estimate
/// plus a total, using the assumed unit prices above. Shared by
/// `get_infrastructure_cost` and `calculate_unit_cost` so the two tools
/// can never silently drift to different cost math.
fn estimate_cost(rows: &[serde_json::Value]) -> (Vec<serde_json::Value>, f64) {
    let mut per_container = Vec::new();
    let mut total_hourly = 0.0;

    for row in rows {
        let name = row["Name"].as_str().unwrap_or("unknown");
        let cpu_percent = row["CPUPerc"]
            .as_str()
            .and_then(parse_percent)
            .unwrap_or(0.0);
        let mem_used_bytes = row["MemUsage"]
            .as_str()
            .and_then(parse_mem_usage_used)
            .unwrap_or(0.0);
        let hourly = (cpu_percent / 100.0) * ASSUMED_VCPU_HOURLY_USD
            + (mem_used_bytes / 1_000_000_000.0) * ASSUMED_GB_HOURLY_USD;
        total_hourly += hourly;
        per_container.push(serde_json::json!({
            "container": name,
            "cpu_percent": cpu_percent,
            "mem_bytes": mem_used_bytes,
            "assumed_hourly_cost_usd": hourly,
        }));
    }

    (per_container, total_hourly)
}

#[derive(Clone)]
struct FinOps {
    records: Arc<Vec<Record>>,
    tags: ProductTags,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl FinOps {
    fn load(ledger_dir: &str, tags_path: &str) -> anyhow::Result<Self> {
        use anyhow::Context;
        // `.with_context` names the actual path in the error -- without
        // it, an io::Error like "No such file or directory" gives no clue
        // which of the two paths (ledger dir vs tags file) was the
        // problem, which is exactly the ambiguity that made this bug slow
        // to track down.
        let outcome = recover_read_only(ledger_dir)
            .with_context(|| format!("failed to recover ledger at '{ledger_dir}'"))?;
        let records: Vec<Record> = outcome
            .records
            .iter()
            .map(|payload| decode_payload(payload))
            .collect::<Result<_, _>>()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let tags = ProductTags::load(tags_path)
            .with_context(|| format!("failed to load tags file at '{tags_path}'"))?;
        Ok(FinOps {
            records: Arc::new(records),
            tags,
            tool_router: Self::tool_router(),
        })
    }

    /// Gross transfer volume for `product`, optionally restricted to a
    /// value_time range. Gross, not net, on purpose: volume/throughput is
    /// "how much money moved," which a net figure can't answer (a product
    /// that moves $1M in and $1M out nets to zero but clearly isn't zero
    /// volume).
    ///
    /// Counts each *transfer* once, not each posting leg. A batch's
    /// transferred amount per denomination is well-defined -- `apply`'s
    /// zero-sum invariant guarantees debits equal credits for that denom
    /// within the batch -- so summing one side (debits) gives the real
    /// transferred amount regardless of how many legs happen to touch a
    /// tagged account. Summing every leg independently instead would
    /// double-count any batch where *both* sides land on the same
    /// product's accounts (an internal transfer), while a transfer
    /// crossing the product's boundary (one leg tagged, one not) would
    /// only count once -- an inconsistency this method exists to avoid.
    fn gross_volume_for_product(
        &self,
        product: &str,
        start: Option<i64>,
        end: Option<i64>,
    ) -> (u128, usize) {
        use ledger_types::ids::DenomId;
        use ledger_types::posting::Side;
        use std::collections::BTreeMap;

        let mut volume: u128 = 0;
        let mut transfer_count = 0usize;

        for record in self.records.iter() {
            if let Command::PostBatch(batch) = record.command() {
                if let Some(s) = start {
                    if batch.value_time.0 < s {
                        continue;
                    }
                }
                if let Some(e) = end {
                    if batch.value_time.0 > e {
                        continue;
                    }
                }

                let mut transferred_by_denom: BTreeMap<DenomId, u128> = BTreeMap::new();
                let mut touched_by_denom: BTreeMap<DenomId, bool> = BTreeMap::new();

                for posting in &batch.postings {
                    if posting.side == Side::Debit {
                        *transferred_by_denom.entry(posting.key.denom).or_insert(0) +=
                            posting.amount;
                    }
                    if self.tags.product_for_account(posting.key.account) == Some(product) {
                        touched_by_denom.insert(posting.key.denom, true);
                    }
                }

                for (denom, amount) in transferred_by_denom {
                    if touched_by_denom.get(&denom).copied().unwrap_or(false) {
                        volume += amount;
                        transfer_count += 1;
                    }
                }
            }
        }

        (volume, transfer_count)
    }

    #[tool(
        description = "Total gross transfer volume for a product (each transfer counted once, using the ledger's zero-sum invariant, not once per posting leg), optionally within a value_time range. Real data from the ledger's own history, via ledger-tags -- not synthetic."
    )]
    fn get_product_volume(
        &self,
        Parameters(GetProductVolumeParams {
            product,
            start_value_time,
            end_value_time,
        }): Parameters<GetProductVolumeParams>,
    ) -> String {
        let (volume, postings) =
            self.gross_volume_for_product(&product, start_value_time, end_value_time);
        serde_json::json!({
            "product": product,
            "start_value_time": start_value_time,
            "end_value_time": end_value_time,
            // u128 as a string: large volumes can exceed what a JSON
            // number safely round-trips through in every consumer.
            "gross_volume": volume.to_string(),
            "transfer_count": postings,
        })
        .to_string()
    }

    #[tool(
        description = "Estimate current infrastructure cost from a live docker stats snapshot (CPU%, memory) for the given containers, converted to a dollar rate using an assumed unit price. SYNTHETIC: there is no real cloud bill on a local machine, and this reflects only a point-in-time snapshot, never historical usage over any period."
    )]
    fn get_infrastructure_cost(
        &self,
        Parameters(GetInfrastructureCostParams { containers }): Parameters<
            GetInfrastructureCostParams,
        >,
    ) -> String {
        let container_list = containers.unwrap_or_default();
        match run_docker_stats(&container_list) {
            Ok(rows) => {
                let (per_container, total_hourly) = estimate_cost(&rows);
                serde_json::json!({
                    "synthetic": true,
                    "cost_basis": format!(
                        "live docker stats snapshot x assumed unit price (${ASSUMED_VCPU_HOURLY_USD}/vCPU-hr, ${ASSUMED_GB_HOURLY_USD}/GB-hr); not a real bill, not historical"
                    ),
                    "per_container": per_container,
                    "total_assumed_hourly_cost_usd": total_hourly,
                })
                .to_string()
            }
            Err(e) => format!("{{\"error\": \"failed to query docker stats: {e}\"}}"),
        }
    }

    #[tool(
        description = "Cost per unit of volume for a product: combines the CURRENT synthetic infrastructure cost rate with the product's all-time real volume. Mixes a point-in-time cost snapshot with all-time volume -- directional only, not a true unit economics figure."
    )]
    fn calculate_unit_cost(
        &self,
        Parameters(CalculateUnitCostParams { product }): Parameters<CalculateUnitCostParams>,
    ) -> String {
        let (volume, _) = self.gross_volume_for_product(&product, None, None);
        if volume == 0 {
            return serde_json::json!({"error": "no volume recorded for this product; cannot compute a unit cost"}).to_string();
        }

        match run_docker_stats(&[]) {
            Ok(rows) => {
                let (_, total_hourly) = estimate_cost(&rows);
                let unit_cost = total_hourly / (volume as f64);
                serde_json::json!({
                    "product": product,
                    "synthetic": true,
                    "approximation_note": "combines a live infra-cost snapshot rate with all-time real volume -- directional only, not a true unit economics figure",
                    "gross_volume": volume.to_string(),
                    "assumed_hourly_cost_usd": total_hourly,
                    "assumed_cost_per_unit_volume_usd": unit_cost,
                })
                .to_string()
            }
            Err(e) => format!("{{\"error\": \"failed to query docker stats: {e}\"}}"),
        }
    }

    #[tool(
        description = "Compare a product's real gross volume between two value_time periods. Entirely real data from the ledger's own history -- no synthetic numbers, unlike the cost-related tools."
    )]
    fn compare_periods(
        &self,
        Parameters(ComparePeriodsParams {
            product,
            period_a_start,
            period_a_end,
            period_b_start,
            period_b_end,
        }): Parameters<ComparePeriodsParams>,
    ) -> String {
        let (vol_a, count_a) =
            self.gross_volume_for_product(&product, Some(period_a_start), Some(period_a_end));
        let (vol_b, count_b) =
            self.gross_volume_for_product(&product, Some(period_b_start), Some(period_b_end));
        let percent_change = if vol_a > 0 {
            Some(((vol_b as f64 - vol_a as f64) / vol_a as f64) * 100.0)
        } else {
            None
        };

        serde_json::json!({
            "product": product,
            "period_a": { "start": period_a_start, "end": period_a_end, "gross_volume": vol_a.to_string(), "transfer_count": count_a },
            "period_b": { "start": period_b_start, "end": period_b_end, "gross_volume": vol_b.to_string(), "transfer_count": count_b },
            "percent_change": percent_change,
        })
        .to_string()
    }
}

#[tool_handler]
impl ServerHandler for FinOps {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "FinOps: get_product_volume and compare_periods are real (ledger history + ledger-tags). \
             get_infrastructure_cost and calculate_unit_cost are SYNTHETIC estimates from live docker stats \
             x an assumed unit price -- always check the synthetic/cost_basis fields before trusting a dollar figure."
                .into(),
        );
        info
    }
}

const REQUIRED_ROLE: &str = "finops";

fn check_role() -> anyhow::Result<()> {
    let role = std::env::var("LEDGER_AGENT_ROLE").unwrap_or_default();
    if role != REQUIRED_ROLE {
        anyhow::bail!(
            "ledger-finops only serves the '{REQUIRED_ROLE}' role, but LEDGER_AGENT_ROLE={role:?}. \
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
        .ok_or_else(|| anyhow::anyhow!("usage: ledger-finops <ledger-dir> <tags-file>"))?;
    let tags_path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: ledger-finops <ledger-dir> <tags-file>"))?;

    let finops = FinOps::load(&ledger_dir, &tags_path)?;
    eprintln!(
        "ledger-finops: loaded {} records from {ledger_dir}, tags from {tags_path}",
        finops.records.len()
    );

    let service = finops.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
