//! mcp.rs — Planetarium's tools for agents, served as a small MCP server (JSON-RPC over HTTP,
//! "streamable HTTP" transport, plain JSON responses only) at http://127.0.0.1:47615/mcp.
//!
//! Tools:
//!   release         "I'm done with these files": drops the agent's holds early.
//!   message_agent   "Let them work it out" mode: the paused agent writes to the holder and
//!                   waits for the answer.
//!   reply_to_agent  the holder answers: go_ahead / wait / done.
//!
//! The MCP request itself doesn't say which agent is calling. Claude Code's hooks do, so the
//! actual work for `release` happens in its PostToolUse hook, and the caller of the messaging
//! tools is taken from their PreToolUse hook (see coord.rs). This file only speaks the protocol.

use serde_json::{json, Value};

pub const PATH: &str = "/mcp";
const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// What to do with one JSON-RPC message.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Send this JSON response now.
    Respond(Value),
    /// A notification or response from the client: answer 202 with no body.
    Accepted,
    /// Run a tool.
    Call { id: Value, name: String, args: Value },
}

pub fn tools() -> Value {
    json!([
        {
            "name": "release",
            "description": "Tell Planetarium you're finished with files you edited, so other agents working in the same repo stop being warned about them. Every file you edit is held until your turn ends; call this when you're done changing a file and won't come back to it this turn. Pass the files (absolute, or relative to your working directory), or all: true.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "files": { "type": "array", "items": { "type": "string" }, "description": "Files you're done with." },
                    "all": { "type": "boolean", "description": "Release every file you're holding." }
                }
            }
        },
        {
            "name": "message_agent",
            "description": "Only when Planetarium has paused one of your edits and given you a collision number: send the agent working on that file a short message saying what you want to change and why. Waits up to 90 seconds for their answer and returns it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "collision": { "type": "integer", "description": "The collision number from Planetarium's message." },
                    "message": { "type": "string", "description": "What you want to change in the file, and why. One or two sentences." }
                },
                "required": ["collision", "message"]
            }
        },
        {
            "name": "reply_to_agent",
            "description": "Only when Planetarium tells you another agent has messaged you about a collision: answer them. decision is \"go_ahead\" (their change is fine), \"wait\" (you still need the file; say why and for about how long), or \"done\" (you've finished with the file; this releases it).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "collision": { "type": "integer" },
                    "decision": { "type": "string", "enum": ["go_ahead", "wait", "done"] },
                    "message": { "type": "string", "description": "A short note for them." }
                },
                "required": ["collision", "decision"]
            }
        }
    ])
}

fn ok(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool result (shown to the agent).
pub fn tool_result(id: &Value, text: &str, is_error: bool) -> Value {
    ok(id, json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
}

/// Decide what to do with one request body.
pub fn handle(body: &str) -> Action {
    let Ok(msg) = serde_json::from_str::<Value>(body) else {
        return Action::Respond(err(&Value::Null, -32700, "Parse error"));
    };
    let Some(method) = msg.get("method").and_then(|m| m.as_str()) else {
        return Action::Accepted; // a response to something we sent (we never do), or junk
    };
    let Some(id) = msg.get("id").cloned() else {
        return Action::Accepted; // notification, e.g. notifications/initialized
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
            let version = if SUPPORTED_VERSIONS.contains(&asked) { asked } else { SUPPORTED_VERSIONS[1] };
            Action::Respond(ok(&id, json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "planetarium", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Planetarium shows the user every coding agent working in their repos and keeps agents from overwriting each other's changes. Use release when you're done with a file you edited. Use message_agent / reply_to_agent only when a Planetarium message asks you to."
            })))
        }
        "ping" => Action::Respond(ok(&id, json!({}))),
        "tools/list" => Action::Respond(ok(&id, json!({ "tools": tools() }))),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            if !["release", "message_agent", "reply_to_agent"].contains(&name.as_str()) {
                return Action::Respond(err(&id, -32602, &format!("Unknown tool: {name}")));
            }
            Action::Call { id, name, args }
        }
        _ => Action::Respond(err(&id, -32601, &format!("Method not found: {method}"))),
    }
}

/// The collision number in a tool's arguments (accepts 7 or "7").
pub fn collision_arg(args: &Value) -> Option<u64> {
    let v = args.get("collision")?;
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().trim_start_matches('#').parse().ok()))
}

pub fn str_arg<'a>(args: &'a Value, key: &str) -> &'a str {
    args.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_basics() {
        let a = handle(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"claude-code","version":"2"}}}"#);
        let Action::Respond(v) = a else { panic!() };
        assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(v["id"], 1);
        assert_eq!(handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#), Action::Accepted);
        let Action::Respond(v) = handle(r#"{"jsonrpc":"2.0","id":"x","method":"tools/list"}"#) else { panic!() };
        assert_eq!(v["result"]["tools"].as_array().unwrap().len(), 3);
        let Action::Call { name, args, .. } = handle(r##"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"message_agent","arguments":{"collision":"#7","message":"hi"}}}"##) else { panic!() };
        assert_eq!((name.as_str(), collision_arg(&args), str_arg(&args, "message")), ("message_agent", Some(7), "hi"));
        let Action::Respond(v) = handle(r#"{"jsonrpc":"2.0","id":3,"method":"nope"}"#) else { panic!() };
        assert_eq!(v["error"]["code"], -32601);
        let Action::Respond(v) = handle("not json") else { panic!() };
        assert_eq!(v["error"]["code"], -32700);
    }
}
