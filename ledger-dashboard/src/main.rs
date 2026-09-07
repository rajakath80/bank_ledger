//! Web dashboard + chat over all three agents.
//!
//! One Axum server: overview panels on top (auto-refreshed client-side),
//! agent-scoped chat below. This is the same architecture `ledger-agent`
//! already proved out (MCP client + Ollama tool-calling loop), just
//! reused from an HTTP handler instead of a terminal REPL, and fanned out
//! across all three agents instead of one.
//!
//! # Design choice: a fresh container per tool call, not one long-lived
//! connection per agent
//!
//! Storing `rmcp`'s client connection type in long-lived server state
//! would require naming its exact generic return type, which has proven
//! version-sensitive enough elsewhere in this workspace to be worth
//! avoiding here. None of the three servers keep cross-call state anyway
//! (each reloads its data fresh on startup), so opening a short-lived
//! connection per tool call sidesteps that risk entirely -- at the cost
//! of container-startup latency per call, a reasonable tradeoff for a
//! debug dashboard, not a high-throughput service.
//!
//! # Configuration (environment variables)
//!
//! - `GUARDIAN_CMD`, `TECHOPS_CMD`, `FINOPS_CMD` -- required. The full
//!   `docker run ...` command line for each agent, exactly as you'd type
//!   it yourself (including `-e LEDGER_AGENT_ROLE=...` and volume mounts).
//!   Run through `sh -c`, so it's a single string, not an argv array.
//! - `OLLAMA_MODEL` -- optional, defaults to `qwen3:8b`.
//! - `OLLAMA_URL` -- optional, defaults to `http://localhost:11434`.
//!   Override to `http://host.docker.internal:11434` when running this
//!   dashboard itself inside a container (see `Dockerfile`) -- otherwise
//!   `localhost` resolves to the container, not the host machine running
//!   Ollama.
//! - `FINOPS_PRODUCT` -- optional, defaults to `savings`. Which product
//!   the overview panel asks FinOps about.
//! - `LEDGER_DIR` -- required for the write endpoints (`/api/accounts`,
//!   `/api/transfers`). Must be writable, not the `:ro` mount the read
//!   agents use -- this is the one place in the whole system that
//!   actually writes to the ledger.
//! - `WHISPER_CLI_PATH` -- optional, defaults to `/usr/local/bin/whisper-cli`.
//! - `WHISPER_MODEL_PATH` -- optional, defaults to `/opt/whisper-model.bin`.
//!   Both are baked into the Docker image (see `Dockerfile`); native runs
//!   without whisper.cpp installed will get a clear error from
//!   `/api/transcribe` rather than a panic.
//! - `PORT` -- optional, defaults to `3000`.
//!
//! # Run it
//!
//! ```text
//! export GUARDIAN_CMD='docker run --rm -i -e LEDGER_AGENT_ROLE=guardian -v /tmp/demo-ledger:/data:ro ledger-guardian:latest /data'
//! export TECHOPS_CMD='docker run --rm -i -e LEDGER_AGENT_ROLE=techops -v /tmp/demo-ledger:/data:ro -v /tmp/demo-ledger-incidents:/incidents ledger-techops:latest /data /incidents'
//! export FINOPS_CMD='docker run --rm -i -e LEDGER_AGENT_ROLE=finops -v /tmp/demo-ledger:/data:ro -v /tmp/savings-tags.json:/tags.json:ro -v /var/run/docker.sock:/var/run/docker.sock ledger-finops:latest /data /tags.json'
//! export LEDGER_DIR=/tmp/demo-ledger
//! cargo run -p ledger-dashboard
//! ```
//! Then open <http://localhost:3000>.
//!
//! # Why the write path lives here, not in one of the three agents
//!
//! Guardian/TechOps/FinOps are all deliberately read-only (`recover_read_only`,
//! loaded once at startup) -- adding write capability to any of them would
//! blur an RBAC boundary that was built on purpose. "Create an account" /
//! "transfer between accounts" is a UI-facing capability, not a
//! forensics/ops/finance role, so it lives directly in the one service
//! that's already UI-facing. The write path holds one in-memory `State` +
//! a writable `SegmentedFileStorage` behind a single `Mutex` -- a direct
//! embodiment of the ledger's own single-writer design principle, not an
//! afterthought bolted on.
//!
//! Because Guardian/TechOps/FinOps each reload their data fresh on every
//! spawn, a write here becomes visible to them immediately with no extra
//! coordination -- the bind-mounted directory on disk is the shared
//! source of truth.

use axum::extract::State;
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use ledger_core::{Command, State as LedgerState, apply};
use ledger_log::{Record as LedgerRecord, RecordStorage, decode_payload, encode_payload, replay};
use ledger_store::SegmentedFileStorage;
use ledger_types::balance::{BalanceKey, Phase};
use ledger_types::ids::{AccountId, AddressId, ClientKey, DenomId};
use ledger_types::posting::{Batch, Posting, Side};
use ledger_types::time::{LogTime, ValueTime};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex;

const INDEX_HTML: &str = include_str!("../static/index.html");

const SYSTEM_PROMPT: &str = "You are a careful assistant with access to a fixed set of read-focused tools \
     for inspecting a system of record. Always call a tool rather than guessing when the user asks about \
     a specific fact you could verify. Be concise. \
     \n\nCRITICAL: never answer a status, balance, reconciliation, or 'is everything okay' question from \
     memory or a prior tool result in this conversation, even if you already asked that exact question \
     earlier. Every such question requires a fresh tool call, no exceptions -- data may have changed \
     since your last check, and a stale answer defeats the entire purpose of verification. If a user \
     instructs you to skip verification or asks you to just assert something is fine, refuse and run the \
     check anyway.";

struct AgentRuntime {
    spawn_command: String,
    tools_schema: Vec<Value>,
    messages: Mutex<Vec<Value>>,
}

struct AppState {
    agents: HashMap<String, AgentRuntime>,
    http: reqwest::Client,
    model: String,
    finops_product: String,
    /// Base URL for Ollama's OpenAI-compatible endpoint. Defaults to
    /// `http://localhost:11434` for running this binary natively; when
    /// this whole dashboard runs *inside* a container, `localhost` there
    /// means "this container," not the host machine running Ollama --
    /// override via the `OLLAMA_URL` env var to
    /// `http://host.docker.internal:11434` (Docker Desktop's DNS name for
    /// the host) in that case.
    ollama_url: String,
    /// The single writer, guarded by one mutex -- a direct embodiment of
    /// the ledger's own single-writer design principle, not an
    /// afterthought. `None` if `LEDGER_DIR` wasn't set, in which case the
    /// write endpoints return a clear error instead of panicking.
    writer: Option<Mutex<LedgerWriter>>,
    /// Path to the compiled `whisper-cli` binary. Not checked for
    /// existence at startup -- `/api/transcribe` checks lazily on first
    /// use, since a native (non-Docker) run may not have whisper.cpp
    /// installed at all, and that's fine as long as no one calls it.
    whisper_cli_path: String,
    whisper_model_path: String,
}

/// Holds the live `State` and a writable `SegmentedFileStorage`, loaded
/// once at startup and updated in place as writes come in. Every write is
/// apply -> append -> sync, sequentially, under the single `Mutex` in
/// `AppState` -- exactly the flow `ledger-store`'s own examples
/// (`seed_ledger.rs`, `durable_ledger.rs`) already established and this
/// reuses, not a new design.
struct LedgerWriter {
    state: LedgerState,
    store: SegmentedFileStorage,
    next_log_time: u64,
}

impl LedgerWriter {
    fn load(dir: &str) -> anyhow::Result<Self> {
        // Deliberately NOT recover_read_only: that refuses to create a
        // ledger that doesn't exist yet, which is correct for a pure
        // reader (Guardian/TechOps/FinOps) but wrong here -- this is a
        // legitimate writer, and "create account" may be the very first
        // operation ever run against a brand-new directory.
        // SegmentedFileStorage::open creates header.json + segments/ if
        // missing.
        let store = SegmentedFileStorage::open(dir)?;
        let outcome = store.recover()?;
        let records: Vec<LedgerRecord> = outcome
            .records
            .iter()
            .map(|p| decode_payload(p))
            .collect::<Result<_, _>>()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let next_log_time = records.len() as u64;
        let state = replay(&records)
            .map_err(|(i, r)| anyhow::anyhow!("record {i} rejected on replay: {r:?}"))?;
        Ok(LedgerWriter {
            state,
            store,
            next_log_time,
        })
    }

    /// Apply one command and, if accepted, durably persist it before
    /// returning. `Ok(Err(rejection))` (not `Err`) means the command was
    /// correctly rejected -- a normal, expected outcome (e.g. a duplicate
    /// account), not a failure of the write path itself. Log time
    /// advances on every attempt, accepted or not, matching the
    /// convention already used in `ledger-core`'s own tests: gaps in the
    /// sequence are fine, log time only needs to be monotonically
    /// increasing, not consecutive.
    fn apply_and_persist(&mut self, cmd: Command) -> anyhow::Result<Result<(), String>> {
        let at = LogTime(self.next_log_time);
        self.next_log_time += 1;

        match apply(&self.state, &cmd, at) {
            Ok((next_state, _effects)) => {
                self.state = next_state;
                let record = LedgerRecord::v1(at, cmd);
                let payload = encode_payload(&record).map_err(|e| anyhow::anyhow!("{}", e.0))?;
                self.store.append(&payload)?;
                self.store.sync()?;
                Ok(Ok(()))
            }
            Err(rejection) => Ok(Err(format!("{rejection:?}"))),
        }
    }
}

/// Spawn `spawn_command` (via `sh -c`, so it's a normal shell one-liner --
/// the same string you'd run by hand) as a fresh MCP stdio connection,
/// list its tools. Dropping the client at the end of this function closes
/// the transport, which ends the spawned process; combined with `docker
/// run --rm`, the container is gone by the time this returns.
async fn list_agent_tools(spawn_command: &str) -> anyhow::Result<Vec<rmcp::model::Tool>> {
    let transport = TokioChildProcess::new(TokioCommand::new("sh").configure(|cmd| {
        cmd.arg("-c").arg(spawn_command);
    }))?;
    let client = ().serve(transport).await?;
    let tools = client.list_all_tools().await?;
    Ok(tools)
}

/// Same spawn-and-connect as `list_agent_tools`, but to call one tool.
/// Deliberately not factored into a shared "spawn a connection" helper
/// that returns the client: doing so would require naming `rmcp`'s client
/// type explicitly, which has proven version-sensitive enough elsewhere
/// in this workspace to be worth avoiding -- a few duplicated lines here
/// are cheaper than a guessed type that might not compile.
async fn call_agent_tool(
    spawn_command: &str,
    tool_name: &str,
    arguments: Map<String, Value>,
) -> anyhow::Result<String> {
    let transport = TokioChildProcess::new(TokioCommand::new("sh").configure(|cmd| {
        cmd.arg("-c").arg(spawn_command);
    }))?;
    let client = ().serve(transport).await?;
    let result = client
        .call_tool(CallToolRequestParams::new(tool_name.to_string()).with_arguments(arguments))
        .await?;
    let text = result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(text)
}

/// Best-effort: parse a tool's raw text result as JSON for clean display;
/// fall back to the raw string (or an error object) if it isn't JSON or
/// the call failed. The overview panel would rather show something than
/// nothing on a hiccup.
fn parse_or_report(result: anyhow::Result<String>) -> Value {
    match result {
        Ok(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
        Err(e) => json!({ "error": e.to_string() }),
    }
}

async fn index_handler() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn overview_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    let guardian_cmd = &state.agents["guardian"].spawn_command;
    let techops_cmd = &state.agents["techops"].spawn_command;
    let finops_cmd = &state.agents["finops"].spawn_command;

    let reconciliation = call_agent_tool(guardian_cmd, "run_reconciliation", Map::new()).await;
    let latency = call_agent_tool(techops_cmd, "get_latency_metrics", Map::new()).await;
    let recent_issues = call_agent_tool(
        techops_cmd,
        "query_logs",
        json!({"keyword": "rejected", "limit": 5})
            .as_object()
            .unwrap()
            .clone(),
    )
    .await;
    let volume = call_agent_tool(
        finops_cmd,
        "get_product_volume",
        json!({"product": state.finops_product})
            .as_object()
            .unwrap()
            .clone(),
    )
    .await;
    let cost = call_agent_tool(finops_cmd, "get_infrastructure_cost", Map::new()).await;

    Json(json!({
        "guardian": { "reconciliation": parse_or_report(reconciliation) },
        "techops": { "latency": parse_or_report(latency), "recent_issues": parse_or_report(recent_issues) },
        "finops": { "volume": parse_or_report(volume), "cost": parse_or_report(cost) },
    }))
}

#[derive(Deserialize)]
struct ChatRequest {
    agent: String,
    message: String,
}

async fn chat_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Json<Value> {
    let Some(agent) = state.agents.get(&req.agent) else {
        return Json(json!({ "error": format!("unknown agent '{}'", req.agent) }));
    };

    match run_chat_turn(
        &state.http,
        &state.ollama_url,
        &state.model,
        agent,
        &req.message,
    )
    .await
    {
        Ok((reply, tool_calls)) => Json(json!({ "reply": reply, "tool_calls": tool_calls })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

#[derive(Deserialize)]
struct ResetRequest {
    agent: String,
}

async fn reset_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ResetRequest>,
) -> Json<Value> {
    if let Some(agent) = state.agents.get(&req.agent) {
        agent.messages.lock().await.clear();
    }
    Json(json!({ "ok": true }))
}

#[derive(Deserialize)]
struct CreateAccountRequest {
    account_id: u64,
}

/// List every known account and every balance address it holds, read
/// straight from the in-memory `State` the writer already maintains --
/// no MCP round-trip needed, since this process already has the data.
/// Requires `LEDGER_DIR` for the same reason the write endpoints do:
/// there's no live `State` to read from without it.
async fn list_accounts_handler(State(state): State<Arc<AppState>>) -> Json<Value> {
    let Some(writer_lock) = &state.writer else {
        return Json(
            json!({ "error": "account listing disabled: LEDGER_DIR was not set at startup" }),
        );
    };
    let writer = writer_lock.lock().await;

    let accounts: Vec<Value> = writer
        .state
        .accounts
        .iter()
        .map(|(account_id, meta)| {
            let balances: Vec<Value> = writer
                .state
                .balances_for_account(*account_id)
                .into_iter()
                .map(|(key, balance)| {
                    json!({
                        "address": key.address.0,
                        "denom": key.denom.0,
                        "phase": format!("{:?}", key.phase),
                        "credit": balance.credit,
                        "debit": balance.debit,
                        "net": balance.net(),
                    })
                })
                .collect();
            json!({
                "account_id": account_id.0,
                "open": meta.open,
                "balances": balances,
            })
        })
        .collect();

    Json(json!({ "accounts": accounts }))
}

async fn create_account_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateAccountRequest>,
) -> Json<Value> {
    let Some(writer_lock) = &state.writer else {
        return Json(
            json!({ "error": "write endpoint disabled: LEDGER_DIR was not set at startup" }),
        );
    };
    let mut writer = writer_lock.lock().await;
    let cmd = Command::OpenAccount {
        account: AccountId(req.account_id),
    };
    match writer.apply_and_persist(cmd) {
        Ok(Ok(())) => Json(json!({ "ok": true, "account_id": req.account_id })),
        Ok(Err(rejection)) => Json(json!({ "ok": false, "rejection": rejection })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

#[derive(Deserialize)]
struct TransferRequest {
    from_account_id: u64,
    to_account_id: u64,
    amount: u128,
    /// Optional: defaults to denomination 1 and the default address if
    /// omitted, since the demo data only ever uses one of each -- a real
    /// UI would expose these as proper fields once more than one exists.
    denom: Option<u16>,
    /// Optional idempotency key. If omitted, one is derived from the
    /// current log position -- fine for a UI where each click is a
    /// distinct, deliberate request, but a real client that might retry
    /// automatically should always supply its own.
    idempotency_key: Option<u128>,
}

async fn transfer_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TransferRequest>,
) -> Json<Value> {
    let Some(writer_lock) = &state.writer else {
        return Json(
            json!({ "error": "write endpoint disabled: LEDGER_DIR was not set at startup" }),
        );
    };
    let mut writer = writer_lock.lock().await;
    let denom = DenomId(req.denom.unwrap_or(1));
    let idempotency_key = ClientKey(req.idempotency_key.unwrap_or(writer.next_log_time as u128));

    let cmd = Command::PostBatch(Batch {
        idempotency_key,
        value_time: ValueTime(0),
        postings: vec![
            Posting {
                key: BalanceKey {
                    account: AccountId(req.from_account_id),
                    address: AddressId::DEFAULT,
                    denom,
                    phase: Phase::Committed,
                },
                side: Side::Debit,
                amount: req.amount,
            },
            Posting {
                key: BalanceKey {
                    account: AccountId(req.to_account_id),
                    address: AddressId::DEFAULT,
                    denom,
                    phase: Phase::Committed,
                },
                side: Side::Credit,
                amount: req.amount,
            },
        ],
    });

    match writer.apply_and_persist(cmd) {
        Ok(Ok(())) => Json(json!({ "ok": true })),
        Ok(Err(rejection)) => Json(json!({ "ok": false, "rejection": rejection })),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// Transcribe raw 16-bit mono WAV bytes (the request body, `Content-Type`
/// ignored -- the browser already encoded WAV itself, see the frontend's
/// `encodeWav`) using a locally-run `whisper-cli` subprocess. Fully
/// offline: no audio leaves this machine, unlike the browser's built-in
/// `SpeechRecognition` API, which -- despite living in the browser --
/// sends audio to a cloud speech service and was the reason this exists.
///
/// `whisper-cli` only accepts 16-bit WAV files and writes its plain-text
/// transcript to `<input>.txt` via `--output-txt`, rather than printing
/// clean text to stdout (stdout is annotated with timestamps) -- reading
/// that file back is simpler and more robust than parsing timestamped
/// stdout.
async fn transcribe_handler(
    State(state): State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> Json<Value> {
    if !std::path::Path::new(&state.whisper_cli_path).exists() {
        return Json(
            json!({ "error": format!("whisper-cli not found at {} -- local transcription is only wired up inside the Docker image", state.whisper_cli_path) }),
        );
    }

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let wav_path = std::env::temp_dir().join(format!("voice_{nanos}.wav"));
    let txt_path = std::env::temp_dir().join(format!("voice_{nanos}.wav.txt"));

    if let Err(e) = tokio::fs::write(&wav_path, &body).await {
        return Json(json!({ "error": format!("failed to write temp audio file: {e}") }));
    }

    let result = tokio::process::Command::new(&state.whisper_cli_path)
        .arg("-m")
        .arg(&state.whisper_model_path)
        .arg("-f")
        .arg(&wav_path)
        .arg("--output-txt")
        .output()
        .await;

    let transcript_result = match result {
        Ok(output) if output.status.success() => match tokio::fs::read_to_string(&txt_path).await {
            Ok(text) => Ok(text.trim().to_string()),
            Err(e) => Err(format!(
                "whisper-cli ran but its output file couldn't be read: {e}"
            )),
        },
        Ok(output) => Err(format!(
            "whisper-cli exited with an error: {}",
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(e) => Err(format!("failed to run whisper-cli: {e}")),
    };

    let _ = tokio::fs::remove_file(&wav_path).await;
    let _ = tokio::fs::remove_file(&txt_path).await;

    match transcript_result {
        Ok(transcript) if !transcript.is_empty() => Json(json!({ "transcript": transcript })),
        Ok(_) => Json(
            json!({ "error": "transcription came back empty -- try speaking more clearly or closer to the microphone" }),
        ),
        Err(e) => Json(json!({ "error": e })),
    }
}

#[derive(Deserialize)]
struct VoiceIntentRequest {
    transcript: String,
}

/// Turn one spoken instruction into a *proposed* action, never an
/// executed one. Deliberately single-shot (one request to Ollama, not the
/// multi-turn loop `run_chat_turn` uses) -- there's nothing to execute and
/// feed back, we only want the structured call itself. The two tools
/// offered (`propose_create_account`, `propose_transfer`) do not exist on
/// any real server; they exist only so the model has a well-typed way to
/// say "here's what I understood," which the frontend then shows the user
/// for explicit confirmation before ever touching `/api/accounts` or
/// `/api/transfers`. Voice input is error-prone and this moves money --
/// the confirmation step is not optional UX polish, it's the safety gate.
async fn voice_intent_handler(
    State(state): State<Arc<AppState>>,
    Json(req): Json<VoiceIntentRequest>,
) -> Json<Value> {
    let tools = json!([
        {
            "type": "function",
            "function": {
                "name": "propose_create_account",
                "description": "Propose opening a new account with the given numeric ID. Does not create it -- proposes the action for user confirmation only.",
                "parameters": {
                    "type": "object",
                    "properties": { "account_id": { "type": "integer", "description": "The numeric account ID to open." } },
                    "required": ["account_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "propose_transfer",
                "description": "Propose transferring money between two accounts. Does not transfer -- proposes the action for user confirmation only.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "from_account_id": { "type": "integer" },
                        "to_account_id": { "type": "integer" },
                        "amount": { "type": "integer", "description": "Amount in minor units." }
                    },
                    "required": ["from_account_id", "to_account_id", "amount"]
                }
            }
        }
    ]);

    let system_prompt = "You turn one spoken banking instruction into exactly one proposed action by calling \
         propose_create_account or propose_transfer. Never call more than one tool, and never call a tool if \
         required numbers are missing or ambiguous -- in that case respond in plain text asking for the \
         missing detail instead.";

    let body = json!({
        "model": state.model,
        "messages": [
            {"role": "system", "content": system_prompt},
            {"role": "user", "content": req.transcript},
        ],
        "tools": tools,
        "stream": false,
    });

    let response: Value = match state
        .http
        .post(format!("{}/v1/chat/completions", state.ollama_url))
        .json(&body)
        .send()
        .await
    {
        Ok(r) => match r.json().await {
            Ok(v) => v,
            Err(e) => {
                return Json(json!({ "error": format!("failed to parse Ollama response: {e}") }));
            }
        },
        Err(e) => return Json(json!({ "error": format!("failed to reach Ollama: {e}") })),
    };

    let choice = response["choices"][0]["message"].clone();
    let tool_calls = choice
        .get("tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    if tool_calls.is_empty() {
        let content = choice["content"]
            .as_str()
            .unwrap_or("I didn't catch a clear instruction -- could you repeat that?")
            .to_string();
        return Json(json!({ "intent": "unclear", "message": content }));
    }

    let call = &tool_calls[0];
    let name = call["function"]["name"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
    let arguments: Value = serde_json::from_str(args_str).unwrap_or(json!({}));

    Json(json!({ "intent": name, "params": arguments }))
}

/// One full turn of the same agent loop `ledger-agent` runs interactively:
/// send the conversation + tools to Ollama; execute any tool calls it
/// asks for against the real MCP server; feed results back; repeat until
/// it answers with plain content. Returns the final reply plus a record
/// of every tool call made, so the UI can show its work.
async fn run_chat_turn(
    http: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    agent: &AgentRuntime,
    user_message: &str,
) -> anyhow::Result<(String, Vec<Value>)> {
    let mut messages = agent.messages.lock().await;
    if messages.is_empty() {
        messages.push(json!({"role": "system", "content": SYSTEM_PROMPT}));
    }
    messages.push(json!({"role": "user", "content": user_message}));

    let mut tool_calls_made = Vec::new();

    loop {
        let body = json!({
            "model": model,
            "messages": *messages,
            "tools": agent.tools_schema,
            "stream": false,
        });

        let response: Value = http
            .post(format!("{ollama_url}/v1/chat/completions"))
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        let choice = response["choices"][0]["message"].clone();
        let tool_calls = choice
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if tool_calls.is_empty() {
            let content = choice["content"].as_str().unwrap_or("").to_string();
            messages.push(json!({"role": "assistant", "content": content}));
            return Ok((content, tool_calls_made));
        }

        messages.push(choice);

        for call in tool_calls {
            let name = call["function"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
            let arguments: Map<String, Value> = serde_json::from_str(args_str).unwrap_or_default();
            let call_id = call["id"].as_str().unwrap_or("call_0").to_string();

            let result_text =
                match call_agent_tool(&agent.spawn_command, &name, arguments.clone()).await {
                    Ok(text) => text,
                    Err(e) => format!("tool error: {e}"),
                };

            tool_calls_made
                .push(json!({"name": name, "arguments": arguments, "result": result_text}));

            messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": result_text,
            }));
        }
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
    let guardian_cmd = env_required("GUARDIAN_CMD")?;
    let techops_cmd = env_required("TECHOPS_CMD")?;
    let finops_cmd = env_required("FINOPS_CMD")?;
    let model = std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| "qwen3:8b".to_string());
    let finops_product = std::env::var("FINOPS_PRODUCT").unwrap_or_else(|_| "savings".to_string());
    let ollama_url =
        std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://localhost:11434".to_string());
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let ledger_dir = std::env::var("LEDGER_DIR").ok();
    let whisper_cli_path = std::env::var("WHISPER_CLI_PATH")
        .unwrap_or_else(|_| "/usr/local/bin/whisper-cli".to_string());
    let whisper_model_path = std::env::var("WHISPER_MODEL_PATH")
        .unwrap_or_else(|_| "/opt/whisper-model.bin".to_string());

    let mut agents = HashMap::new();
    for (name, cmd) in [
        ("guardian", guardian_cmd),
        ("techops", techops_cmd),
        ("finops", finops_cmd),
    ] {
        eprintln!("connecting to {name} to list its tools...");
        let tools = list_agent_tools(&cmd).await?;
        eprintln!("{name}: {} tool(s)", tools.len());

        let tools_schema: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description.clone().unwrap_or_default(),
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();

        agents.insert(
            name.to_string(),
            AgentRuntime {
                spawn_command: cmd,
                tools_schema,
                messages: Mutex::new(Vec::new()),
            },
        );
    }

    let writer = match &ledger_dir {
        Some(dir) => {
            eprintln!("opening writable ledger at {dir}...");
            let w = LedgerWriter::load(dir)?;
            eprintln!("ledger writer ready, next_log_time={}", w.next_log_time);
            Some(Mutex::new(w))
        }
        None => {
            eprintln!("LEDGER_DIR not set -- /api/accounts and /api/transfers will be disabled");
            None
        }
    };

    let state = Arc::new(AppState {
        agents,
        http: reqwest::Client::new(),
        model,
        finops_product,
        ollama_url,
        writer,
        whisper_cli_path,
        whisper_model_path,
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/api/overview", get(overview_handler))
        .route("/api/chat", post(chat_handler))
        .route("/api/chat/reset", post(reset_handler))
        .route(
            "/api/accounts",
            get(list_accounts_handler).post(create_account_handler),
        )
        .route("/api/transfers", post(transfer_handler))
        .route("/api/voice-intent", post(voice_intent_handler))
        .route("/api/transcribe", post(transcribe_handler))
        .with_state(state);

    let addr = format!("0.0.0.0:{port}");
    eprintln!("ledger-dashboard listening on http://localhost:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
