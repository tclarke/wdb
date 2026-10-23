//! Minimal MCP server: newline-delimited JSON-RPC 2.0 over stdio, exposing
//! one tool (`wdb`) that runs a debugger command and returns its plain-text output.

use crate::debugger::Debugger;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const DEFAULT_PROTOCOL: &str = "2025-06-18";

pub fn serve(mut dbg: Debugger) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&mut dbg, &msg),
            Err(e) => Some(error(Value::Null, -32700, &format!("parse error: {}", e))),
        };
        if let Some(reply) = reply
            && writeln!(stdout, "{}", reply).and_then(|_| stdout.flush()).is_err() {
                break;
            }
    }
}

/// Handle one JSON-RPC message. Returns None for notifications.
fn handle(dbg: &mut Debugger, msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(DEFAULT_PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "wdb", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "Use the `wdb` tool to drive a WITCH computer debugger. \
                Start with `load <tape-file>` then `reset`, then `run`, `step`, `dump`, `print <loc>`.",
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": [tool_def()] }),
        "tools/call" => call_tool(dbg, &params),
        _ => return Some(error(id, -32601, &format!("method not found: {}", method))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tool_def() -> Value {
    let description = format!(
        "Run one wdb (WITCH computer debugger) command and return its plain-text output. \
         Debugger state persists between calls. `run` stops after a step limit; call it again to continue.\n{}",
        Debugger::cmd_help().join("\n")
    );
    json!({
        "name": "wdb",
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "debugger command, e.g. `load prog.tape`, `step`, `dump`" }
            },
            "required": ["command"],
        },
    })
}

fn call_tool(dbg: &mut Debugger, params: &Value) -> Value {
    if params["name"] != "wdb" {
        return tool_result(format!("unknown tool: {}", params["name"]), true);
    }
    let Some(command) = params["arguments"]["command"].as_str() else {
        return tool_result("missing string argument `command`".into(), true);
    };
    let first = command.split_whitespace().next().unwrap_or("");
    if matches!(first, "quit" | "exit" | "q") {
        return tool_result("quit is not available in MCP mode".into(), true);
    }
    tool_result(dbg.execute(command).join("\n"), false)
}

fn tool_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_roundtrip() {
        colored::control::set_override(false);
        let mut dbg = Debugger::new();

        let r = handle(&mut dbg, &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}})).unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(r["result"]["serverInfo"]["name"], "wdb");

        assert!(handle(&mut dbg, &json!({"jsonrpc":"2.0","method":"notifications/initialized"})).is_none());

        let r = handle(&mut dbg, &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).unwrap();
        assert_eq!(r["result"]["tools"][0]["name"], "wdb");

        let call = |dbg: &mut Debugger, cmd: &str| {
            handle(dbg, &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"wdb","arguments":{"command":cmd}}})).unwrap()
        };
        let r = call(&mut dbg, "print acc");
        assert_eq!(r["result"]["isError"], false);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().starts_with("acc = "));
        assert_eq!(call(&mut dbg, "quit")["result"]["isError"], true);

        let r = handle(&mut dbg, &json!({"jsonrpc":"2.0","id":4,"method":"bogus"})).unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }
}
