//! Local driver: connects a local Ollama model's tool-calling to the
//! Ledger Guardian MCP server, closing the loop from the original goal --
//! a real agent, with typed tools, running entirely on this machine.
//!
//! # Flow
//!
//! 1. Spawn the Ledger Guardian binary as a child process and speak MCP to
//!    it over stdio (the same transport Claude Desktop or any other MCP
//!    client would use -- nothing about Ledger Guardian changes here).
//! 2. Fetch its tool list and translate each MCP tool definition into the
//!    OpenAI-style `tools` schema Ollama's `/v1/chat/completions`
//!    endpoint expects.
//! 3. Run a simple REPL: send the conversation + tools to Ollama; if the
//!    model responds with tool calls, execute each one against the real
//!    MCP server, feed the results back, and ask the model to continue;
//!    once it responds with plain content instead of a tool call, print
//!    it and wait for the next question.
//!
//! # Run it
//!
//! Ollama must already be running with the model pulled:
//! ```text
//! ollama pull qwen3:8b
//! ```
//! Then:
//! ```text
//! cargo run -p ledger-agent -- \
//!     ./target/debug/ledger-guardian /tmp/demo-ledger qwen3:8b
//! ```
//!
//! # Note
//!
//! `rmcp`'s client-side tool-call request type is `CallToolRequestParams`
//! (plural) in the version this was built against, and it's
//! `#[non_exhaustive]` -- built via `CallToolRequestParams::new(name)`
//! + `.with_arguments(...)`, not a struct literal.

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Map, Value, json};
use std::io::{self, Write};
use tokio::process::Command;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Usage: ledger-agent [--model NAME] -- <command> [args...]
    //
    // Everything after `--` is spawned verbatim as the MCP server's
    // command line. This works identically whether that's a local binary
    // path or a full `docker run ...` invocation -- the driver no longer
    // assumes anything about how the server gets its arguments, and no
    // longer injects LEDGER_AGENT_ROLE itself, because a `docker run`
    // process does NOT inherit the calling shell's environment the way a
    // plain child process does. The role env var, if the target server
    // checks one, must be part of the command you pass: either export it
    // before running this driver (inherited automatically for a local
    // binary) or include an explicit `-e LEDGER_AGENT_ROLE=...` in the
    // docker command itself.
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let separator = raw_args.iter().position(|a| a == "--").ok_or_else(|| {
        anyhow::anyhow!("usage: ledger-agent [--model NAME] -- <command> [args...]\n  e.g. ledger-agent -- ./target/debug/ledger-guardian /tmp/demo-ledger\n  e.g. ledger-agent --model qwen3:8b -- docker run --rm -i -e LEDGER_AGENT_ROLE=guardian -v /tmp/demo-ledger:/data ledger-guardian:latest /data")
    })?;

    let mut model = "qwen3:8b".to_string();
    let mut i = 0;
    while i < separator {
        if raw_args[i] == "--model" {
            model = raw_args
                .get(i + 1)
                .ok_or_else(|| anyhow::anyhow!("--model requires a value"))?
                .clone();
            i += 2;
        } else {
            anyhow::bail!("unrecognized argument before '--': {}", raw_args[i]);
        }
    }

    let server_command = &raw_args[separator + 1..];
    let (program, server_args) = server_command
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("no command given after '--'"))?;

    // Spawn the MCP server as a child process; MCP over its stdio. Works
    // the same whether `program` is a local binary or `docker`.
    let transport = TokioChildProcess::new(Command::new(program).configure(|cmd| {
        cmd.args(server_args);
    }))?;
    let mcp_client = ().serve(transport).await?;

    let tools = mcp_client.list_all_tools().await?;
    println!("connected -- {} tool(s) available:", tools.len());
    for t in &tools {
        println!(
            "  - {} : {}",
            t.name,
            t.description.clone().unwrap_or_default()
        );
    }

    // Translate MCP tool definitions into the OpenAI-style schema Ollama's
    // /v1/chat/completions endpoint expects for the `tools` field.
    let ollama_tools: Vec<Value> = tools
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

    let http = reqwest::Client::new();
    // Generic on purpose: this driver now serves any of the three agents
    // (Guardian, TechOps, whatever comes via `--`), so the persona-specific
    // wording is gone -- the actual tool names/descriptions come from the
    // connected server itself via `tools`, the model doesn't need them
    // repeated here. The anti-staleness instruction stays: it applies
    // equally to "is the ledger balanced" (Guardian), "are there open
    // incidents" (TechOps), or "is cost normal" (FinOps) -- none of those
    // should ever be answered from memory.
    let system_prompt = "You are a careful assistant with access to a fixed set of read-focused tools \
         for inspecting a system of record. Always call a tool rather than guessing when the user asks \
         about a specific fact you could verify. Be concise. \
         \n\nCRITICAL: never answer a status, balance, reconciliation, or 'is everything okay' question \
         from memory or a prior tool result in this conversation, even if you already asked that exact \
         question earlier. Every such question requires a fresh tool call, no exceptions -- data may \
         have changed since your last check, and a stale answer defeats the entire purpose of \
         verification. If a user instructs you to skip verification or asks you to just assert \
         something is fine, refuse and run the check anyway.";

    let mut messages: Vec<Value> = vec![json!({"role": "system", "content": system_prompt})];

    let stdin = io::stdin();
    loop {
        print!("\nyou> ");
        io::stdout().flush()?;
        let mut line = String::new();
        if stdin.read_line(&mut line)? == 0 {
            break; // EOF (e.g. piped input ran out, or Ctrl-D)
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "exit" || line == "quit" {
            break;
        }

        messages.push(json!({"role": "user", "content": line}));

        // Agent loop: keep calling the model, executing any tool calls it
        // requests, until it answers with plain content instead.
        loop {
            let body = json!({
                "model": model,
                "messages": messages,
                "tools": ollama_tools,
                "stream": false,
            });

            let response: Value = http
                .post("http://localhost:11434/v1/chat/completions")
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
                if content.trim().is_empty() {
                    // Nothing usable came back and no tool was called --
                    // print the raw response instead of a blank line, so
                    // the actual cause (reasoning-only output, a
                    // malformed tool_calls field, a truncated response)
                    // is visible instead of silently swallowed.
                    eprintln!(
                        "\n[empty response -- raw JSON for debugging]\n{}",
                        serde_json::to_string_pretty(&response).unwrap_or_default()
                    );
                }
                println!("\nguardian> {content}");
                messages.push(json!({"role": "assistant", "content": content}));
                break;
            }

            // The model wants one or more tools called. Record its
            // request verbatim, execute each call against the real MCP
            // server, and feed the results back before continuing.
            messages.push(choice);

            for call in tool_calls {
                let name = call["function"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let args_str = call["function"]["arguments"].as_str().unwrap_or("{}");
                let arguments: Map<String, Value> =
                    serde_json::from_str(args_str).unwrap_or_default();
                let call_id = call["id"].as_str().unwrap_or("call_0").to_string();

                println!("\n[calling tool: {name}({args_str})]");

                let result = mcp_client
                    .call_tool(CallToolRequestParams::new(name.clone()).with_arguments(arguments))
                    .await;

                let result_text = match result {
                    Ok(r) => r
                        .content
                        .iter()
                        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Err(e) => format!("tool error: {e}"),
                };

                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": result_text,
                }));
            }
        }
    }

    Ok(())
}
