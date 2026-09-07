//! `ledger-credit`'s HTTP service. Plain Axum, not MCP -- see
//! `04-credit-decisioning-design.md` section 5: every endpoint here is a
//! known operation with known parameters, the same shape as
//! `ledger-dashboard`'s `/api/accounts`/`/api/transfers`, not an
//! open-ended question a model needs to compose on the fly.
//!
//! # Configuration (environment variables)
//!
//! - `LEDGER_DIR` -- required. Read-only; this service never writes to
//!   the real ledger.
//! - `EXTERNAL_DATA_PATH` -- optional. If unset, or set to a path that
//!   doesn't exist, falls back to a mock external-data file baked into
//!   the binary at compile time -- survives a `/tmp` wipe, a fresh git
//!   clone, anything, the same way `ledger-dashboard`'s embedded HTML
//!   does. Set this only if you want different mock data without
//!   recompiling.
//! - `AUDIT_DIR` -- required. Where decisions are durably recorded (a
//!   separate store from the ledger, same pattern as TechOps's incidents).
//! - `PORT` -- optional, defaults to `3001`.
//!
//! # Run it
//!
//! ```text
//! export LEDGER_DIR=/tmp/demo-ledger
//! export AUDIT_DIR=/tmp/demo-ledger-credit-decisions
//! cargo run -p ledger-credit
//! # EXTERNAL_DATA_PATH is optional -- omit it to use the embedded default.
//! ```

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use ledger_credit::audit::{self, Decision};
use ledger_credit::scoring::{self, ScoreResult};
use ledger_credit::signals::{self, ExternalSignals, InternalSignals};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

struct AppState {
    ledger_dir: String,
    external_data_path: Option<String>,
    audit_dir: String,
}

#[derive(Deserialize)]
struct AccountIdRequest {
    account_id: u64,
}

async fn internal_signals_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AccountIdRequest>,
) -> Json<Value> {
    match signals::compute_internal_signals(&state.ledger_dir, req.account_id) {
        Ok(s) => Json(serde_json::to_value(s).unwrap()),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

async fn external_signals_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AccountIdRequest>,
) -> Json<Value> {
    match signals::load_external_signals(state.external_data_path.as_deref(), req.account_id) {
        Ok(s) => Json(serde_json::to_value(s).unwrap()),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

#[derive(Deserialize)]
struct EvaluateRequest {
    account_id: u64,
    requested_amount: u128,
}

#[derive(Serialize)]
struct EvaluateResponse {
    decision_id: u128,
    approved: bool,
    total_score: i32,
    reason_codes: Vec<String>,
    factors: Vec<scoring::ScoreFactor>,
    internal_signals: InternalSignals,
    external_signals: ExternalSignals,
}

async fn evaluate_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<EvaluateRequest>,
) -> Json<Value> {
    let internal = match signals::compute_internal_signals(&state.ledger_dir, req.account_id) {
        Ok(s) => s,
        Err(e) => {
            return Json(json!({ "error": format!("failed to compute internal signals: {e}") }));
        }
    };
    let external =
        match signals::load_external_signals(state.external_data_path.as_deref(), req.account_id) {
            Ok(s) => s,
            Err(e) => {
                return Json(json!({ "error": format!("failed to load external signals: {e}") }));
            }
        };

    let result: ScoreResult = scoring::score(&internal, &external, req.requested_amount);
    let decision = Decision::new(
        req.account_id,
        req.requested_amount,
        internal.clone(),
        external.clone(),
        &result,
    );

    if let Err(e) = audit::persist_decision(&state.audit_dir, &decision) {
        return Json(
            json!({ "error": format!("decision computed but failed to persist to audit log: {e}") }),
        );
    }

    Json(
        serde_json::to_value(EvaluateResponse {
            decision_id: decision.decision_id,
            approved: result.approved,
            total_score: result.total_score,
            reason_codes: result.reason_codes,
            factors: result.factors,
            internal_signals: internal,
            external_signals: external,
        })
        .unwrap(),
    )
}

async fn list_decisions_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    match audit::load_all_decisions(&state.audit_dir) {
        Ok(decisions) => Json(json!({ "decisions": decisions })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

async fn explain_decision_handler(
    State(state): State<Arc<AppState>>,
    Path(decision_id): Path<u128>,
) -> Json<Value> {
    match audit::find_decision(&state.audit_dir, decision_id) {
        Ok(Some(decision)) => Json(serde_json::to_value(decision).unwrap()),
        Ok(None) => Json(json!({ "error": format!("no decision found with id {decision_id}") })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

fn env_required(key: &str) -> anyhow::Result<String> {
    std::env::var(key).map_err(|_| {
        anyhow::anyhow!(
            "{key} environment variable is required (see main.rs's doc comment for an example)"
        )
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let ledger_dir = env_required("LEDGER_DIR")?;
    let external_data_path = std::env::var("EXTERNAL_DATA_PATH").ok();
    let audit_dir = env_required("AUDIT_DIR")?;
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3001);

    let state = Arc::new(AppState {
        ledger_dir,
        external_data_path,
        audit_dir,
    });

    let app = Router::new()
        .route("/signals/internal", post(internal_signals_handler))
        .route("/signals/external", post(external_signals_handler))
        .route("/evaluate", post(evaluate_handler))
        .route("/decisions", get(list_decisions_handler))
        .route("/decisions/{id}", get(explain_decision_handler))
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    eprintln!("ledger-credit listening on http://localhost:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
