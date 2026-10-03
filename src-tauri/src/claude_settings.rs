//! claude_settings.rs — connect Planetarium to Claude Code, and disconnect it again.
//!
//! Connect makes three changes, and Disconnect removes exactly those:
//!   1. HTTP hooks in ~/.claude/settings.json, so Planetarium sees what agents do;
//!   2. "mcp__planetarium" in that file's permissions.allow, so Planetarium's own tools
//!      (release, message_agent, reply_to_agent) don't prompt you each time. They only talk to
//!      Planetarium; they can't read or change your files;
//!   3. Planetarium's tool server in ~/.claude.json ("mcpServers"), where Claude Code keeps
//!      user-wide MCP servers;
//!   4. a status line (only if you don't have one already): it passes Claude Code's usage-limit
//!      numbers to Planetarium for the usage meter, and shows them under the prompt.
//!
//! Rules this follows:
//!   - back up each file before changing it (<name>.planetarium-backup)
//!   - only ever add or remove Planetarium's own entries; nothing else is touched
//!   - keep each file's existing key order (serde_json "preserve_order")
//! Hook changes reach running Claude Code sessions straight away; the tools only appear in
//! sessions started after connecting.

use crate::server::hook_url;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::fs;
use std::path::PathBuf;

/// Tools whose file paths we want to see before they run. Bash is left out on purpose (no file
/// path, and it would add a check before every command).
const FILE_TOOLS: &str = "Read|Edit|MultiEdit|Write|NotebookEdit|Glob|Grep";

/// PreToolUse may wait for your decision in Ask mode (Planetarium's own deadline is shorter:
/// settings.rs caps it at 540 s), so its hook timeout is long. Every other hook answers at once.
const PRE_TOOL_TIMEOUT: u64 = 600;

/// The tool that starts a subagent (named "Task" or "Agent" depending on the version).
const SUBAGENT_TOOLS: &str = "Task|Agent";
/// Planetarium's own tools as hook matchers see them.
const OUR_TOOLS: &str = "mcp__planetarium__.*";
/// Permission rule allowing all of Planetarium's tools.
const ALLOW_RULE: &str = "mcp__planetarium";
const MCP_NAME: &str = "planetarium";

/// (event, matcher) pairs Planetarium registers.
const EVENTS: &[(&str, Option<&str>)] = &[
    ("SessionStart", None),
    ("UserPromptSubmit", None),
    ("PreToolUse", Some(FILE_TOOLS)),
    // Every tool, so messages waiting for an agent reach it even when it's only running commands.
    ("PostToolUse", None),
    ("SubagentStart", None),
    ("SubagentStop", None),
    ("Stop", None),
    ("SessionEnd", None),
    // Starting a subagent: what it's been asked to do (shown under its name).
    ("PreToolUse", Some(SUBAGENT_TOOLS)),
    // Planetarium's own tools: who's calling (Pre) and what a release covered (Post).
    ("PreToolUse", Some(OUR_TOOLS)),
    ("PostToolUse", Some(OUR_TOOLS)),
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeStatus {
    pub connected: bool,
    /// Some but not all of Planetarium's hooks are present (e.g. hand-edited).
    pub partial: bool,
    /// Connected, but with hooks from an older Planetarium (e.g. too short a timeout for Ask mode).
    pub outdated: bool,
    pub settings_path: String,
    pub hook_url: String,
    /// "ours" | "theirs" (you have your own; Planetarium leaves it alone) | "none"
    pub status_line: String,
}

fn home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn settings_path() -> PathBuf {
    home().join(".claude").join("settings.json")
}

/// Where Claude Code keeps user-wide MCP servers.
pub fn claude_json_path() -> PathBuf {
    home().join(".claude.json")
}

pub fn mcp_url() -> String {
    format!("http://127.0.0.1:{}{}", crate::server::PORT, crate::mcp::PATH)
}

fn has_mcp(claude_json: &Value) -> bool {
    claude_json.get("mcpServers").and_then(|m| m.get(MCP_NAME)).and_then(|s| s.get("url")).and_then(|u| u.as_str()) == Some(mcp_url().as_str())
}

/// Planetarium's status line: posts Claude Code's status JSON to Planetarium and prints the
/// reply. If Planetarium isn't running, curl prints nothing and the line is just empty.
fn status_line_command() -> String {
    format!("curl -s --max-time 1 --data-binary @- {}", crate::server::statusline_url())
}

fn status_line_kind(settings: &Value) -> &'static str {
    match settings.get("statusLine") {
        None | Some(Value::Null) => "none",
        Some(v) if v.get("command").and_then(|c| c.as_str()).map_or(false, |c| c.contains(crate::server::STATUSLINE_PATH)) => "ours",
        Some(_) => "theirs",
    }
}

fn has_allow(settings: &Value) -> bool {
    settings.get("permissions").and_then(|p| p.get("allow")).and_then(|a| a.as_array()).map_or(false, |a| a.iter().any(|r| r.as_str() == Some(ALLOW_RULE)))
}

fn read_settings(path: &PathBuf) -> Result<Value, String> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => serde_json::from_str(&text).map_err(|e| {
            format!("Your Claude Code settings file isn't valid JSON, so Planetarium left it alone ({e}).")
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("Couldn't read {}: {e}", path.display())),
    }
}

fn write_settings(path: &PathBuf, value: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if path.exists() {
        let backup = path.with_file_name(format!("{name}.planetarium-backup"));
        fs::copy(path, &backup).map_err(|e| format!("Couldn't back up {name}: {e}"))?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n";
    let tmp = path.with_file_name(format!("{name}.planetarium-tmp"));
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn is_ours(handler: &Value) -> bool {
    handler.get("url").and_then(|u| u.as_str()).map_or(false, |u| u.contains("/planetarium/hook"))
}

/// The longest timeout among Planetarium's handlers for `event` (0 if none).
fn our_timeout(settings: &Value, event: &str) -> u64 {
    settings
        .get("hooks")
        .and_then(|h| h.get(event))
        .and_then(|groups| groups.as_array())
        .map(|groups| {
            groups
                .iter()
                .filter_map(|g| g.get("hooks").and_then(|h| h.as_array()))
                .flatten()
                .filter(|h| is_ours(h))
                .filter_map(|h| h.get("timeout").and_then(|t| t.as_u64()))
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

fn has_any_our_hook(settings: &Value, event: &str) -> bool {
    settings
        .get("hooks")
        .and_then(|h| h.get(event))
        .and_then(|groups| groups.as_array())
        .map_or(false, |groups| groups.iter().any(|g| g.get("hooks").and_then(|h| h.as_array()).map_or(false, |hs| hs.iter().any(is_ours))))
}

fn has_our_hook(settings: &Value, event: &str, matcher: Option<&str>) -> bool {
    settings
        .get("hooks")
        .and_then(|h| h.get(event))
        .and_then(|groups| groups.as_array())
        .map_or(false, |groups| {
            groups.iter().any(|g| {
                let m = g.get("matcher").and_then(|m| m.as_str()).filter(|m| !m.is_empty() && *m != "*");
                m == matcher && g.get("hooks").and_then(|h| h.as_array()).map_or(false, |hs| hs.iter().any(is_ours))
            })
        })
}

pub fn status() -> ClaudeStatus {
    let path = settings_path();
    let status_line = read_settings(&path).map(|v| status_line_kind(&v).to_string()).unwrap_or_else(|_| "none".into());
    let (connected, partial, outdated) = match read_settings(&path) {
        Ok(v) => {
            // "Connected" = Planetarium's hooks are there for every event it needs (from any
            // version). Anything older than what Connect writes today shows as "outdated" and
            // is fixed by connecting again.
            let mut events: Vec<&str> = EVENTS.iter().map(|(e, _)| *e).collect();
            events.sort();
            events.dedup();
            let present = events.iter().filter(|e| has_any_our_hook(&v, e)).count();
            let connected = present == events.len();
            let all_current = EVENTS.iter().all(|(e, m)| has_our_hook(&v, e, *m));
            let tools = has_allow(&v) && read_settings(&claude_json_path()).map_or(false, |c| has_mcp(&c));
            let current = all_current && tools && our_timeout(&v, "PreToolUse") >= PRE_TOOL_TIMEOUT && status_line_kind(&v) != "none";
            (connected, present > 0 && !connected, connected && !current)
        }
        Err(_) => (false, false, false),
    };
    ClaudeStatus { connected, partial, outdated, settings_path: path.display().to_string(), hook_url: hook_url(), status_line }
}

fn strip_status_line(settings: &mut Value) {
    if status_line_kind(settings) == "ours" {
        settings.as_object_mut().map(|o| o.remove("statusLine"));
    }
}

fn strip_allow(settings: &mut Value) {
    let Some(perms) = settings.get_mut("permissions").and_then(|p| p.as_object_mut()) else { return };
    if let Some(allow) = perms.get_mut("allow").and_then(|a| a.as_array_mut()) {
        allow.retain(|r| r.as_str() != Some(ALLOW_RULE));
        if allow.is_empty() {
            perms.remove("allow");
        }
    }
    if perms.is_empty() {
        settings.as_object_mut().map(|o| o.remove("permissions"));
    }
}

/// Add or remove Planetarium's tool server in ~/.claude.json. Only that one entry is touched.
fn set_mcp(on: bool) -> Result<(), String> {
    let path = claude_json_path();
    if !on && !path.exists() {
        return Ok(());
    }
    let mut v = read_settings(&path).map_err(|_| "Claude Code's ~/.claude.json isn't valid JSON, so Planetarium left it alone.".to_string())?;
    let Some(obj) = v.as_object_mut() else { return Err("~/.claude.json isn't a JSON object, so Planetarium left it alone.".into()) };
    if on {
        let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
        let Some(servers) = servers.as_object_mut() else { return Err("\"mcpServers\" in ~/.claude.json isn't an object, so Planetarium left it alone.".into()) };
        let entry = json!({ "type": "http", "url": mcp_url() });
        if servers.get(MCP_NAME) == Some(&entry) {
            return Ok(());
        }
        servers.insert(MCP_NAME.into(), entry);
    } else {
        let Some(servers) = obj.get_mut("mcpServers").and_then(|m| m.as_object_mut()) else { return Ok(()) };
        if servers.remove(MCP_NAME).is_none() {
            return Ok(());
        }
        if servers.is_empty() {
            obj.remove("mcpServers");
        }
    }
    write_settings(&path, &v)
}

/// Remove every Planetarium handler; drop matcher groups and events that end up empty.
fn strip_ours(settings: &mut Value) {
    let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) else { return };
    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        let Some(groups) = hooks.get_mut(&event).and_then(|g| g.as_array_mut()) else { continue };
        for g in groups.iter_mut() {
            if let Some(hs) = g.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                hs.retain(|h| !is_ours(h));
            }
        }
        groups.retain(|g| g.get("hooks").and_then(|h| h.as_array()).map_or(true, |hs| !hs.is_empty()));
        if groups.is_empty() {
            hooks.remove(&event);
        }
    }
    if hooks.is_empty() {
        if let Some(obj) = settings.as_object_mut() {
            obj.remove("hooks");
        }
    }
}

pub fn connect() -> Result<ClaudeStatus, String> {
    let path = settings_path();
    let mut settings = read_settings(&path)?;
    if !settings.is_object() {
        return Err("Your Claude Code settings file isn't a JSON object, so Planetarium left it alone.".into());
    }
    strip_ours(&mut settings); // start clean so reconnecting never duplicates entries
    strip_allow(&mut settings);
    let obj = settings.as_object_mut().unwrap();
    let hooks = obj.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        return Err("The \"hooks\" entry in your Claude Code settings isn't an object, so Planetarium left it alone.".into());
    }
    let hooks = hooks.as_object_mut().unwrap();
    for (event, matcher) in EVENTS {
        let mut group = Map::new();
        if let Some(m) = matcher {
            group.insert("matcher".into(), json!(m));
        }
        // Planetarium answers instantly, and if it's closed the request is refused instantly too.
        // The one long wait is PreToolUse in Ask mode, where an edit pauses for your decision.
        let handler = if *event == "PreToolUse" && *matcher == Some(FILE_TOOLS) {
            json!({ "type": "http", "url": hook_url(), "timeout": PRE_TOOL_TIMEOUT, "statusMessage": "Checking with Planetarium" })
        } else {
            json!({ "type": "http", "url": hook_url(), "timeout": 5 })
        };
        group.insert("hooks".into(), json!([handler]));
        let list = hooks.entry(event.to_string()).or_insert_with(|| json!([]));
        match list.as_array_mut() {
            Some(arr) => arr.push(Value::Object(group)),
            None => return Err(format!("The \"{event}\" hooks entry in your Claude Code settings isn't a list, so Planetarium left it alone.")),
        }
    }
    // Let Planetarium's own tools run without a prompt each time (they only talk to Planetarium).
    let obj = settings.as_object_mut().unwrap();
    // The usage meter's data source, unless you already have a status line of your own.
    if matches!(obj.get("statusLine"), None | Some(Value::Null)) || obj.get("statusLine").and_then(|v| v.get("command")).and_then(|c| c.as_str()).map_or(false, |c| c.contains(crate::server::STATUSLINE_PATH)) {
        obj.insert("statusLine".into(), json!({ "type": "command", "command": status_line_command() }));
    }
    let perms = obj.entry("permissions").or_insert_with(|| json!({}));
    if let Some(perms) = perms.as_object_mut() {
        let allow = perms.entry("allow").or_insert_with(|| json!([]));
        if let Some(a) = allow.as_array_mut() {
            a.push(json!(ALLOW_RULE));
        }
    }
    write_settings(&path, &settings)?;
    set_mcp(true)?;
    Ok(status())
}

pub fn disconnect() -> Result<ClaudeStatus, String> {
    let path = settings_path();
    if !path.exists() {
        set_mcp(false)?;
        return Ok(status());
    }
    let mut settings = read_settings(&path)?;
    strip_ours(&mut settings);
    strip_allow(&mut settings);
    strip_status_line(&mut settings);
    write_settings(&path, &settings)?;
    set_mcp(false)?;
    Ok(status())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    #[test]
    fn claude_settings_connect_and_disconnect_preserve_user_hooks() {
        let home = std::env::temp_dir().join(format!("pl-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let original = r#"{
      "model": "opus",
      "hooks": {
        "PreToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "my-guard.sh" } ] } ]
      },
      "permissions": { "allow": ["Read"] }
    }"#;
        std::fs::write(home.join(".claude/settings.json"), original).unwrap();
        let claude_json = r#"{"numStartups": 41, "mcpServers": {"github": {"type": "http", "url": "https://example.test/mcp"}}, "projects": {"C:/x": {"allowedTools": []}}}"#;
        std::fs::write(home.join(".claude.json"), claude_json).unwrap();
        std::env::set_var("USERPROFILE", &home);
        std::env::set_var("HOME", &home);

        use crate::claude_settings as cs;
        let s = cs::connect().unwrap();
        assert!(s.connected && !s.outdated && !s.partial);
        let after: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap()).unwrap();
        println!("{}", serde_json::to_string_pretty(&after).unwrap());
        assert_eq!(after["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "my-guard.sh");
        assert_eq!(after["hooks"]["PreToolUse"][1]["hooks"][0]["timeout"], 600);
        assert!(home.join(".claude/settings.json.planetarium-backup").exists());
        // Key order kept: model first.
        assert!(std::fs::read_to_string(home.join(".claude/settings.json")).unwrap().trim_start().starts_with("{\n  \"model\""));
        // Connecting twice doesn't duplicate.
        cs::connect().unwrap();
        let again: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap()).unwrap();
        // user's group + Planetarium's file-tools, subagent-start and own-tools groups
        assert_eq!(again["hooks"]["PreToolUse"].as_array().unwrap().len(), 4);
        assert_eq!(again["permissions"]["allow"], serde_json::json!(["Read", "mcp__planetarium"]));
        let cj: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
        assert_eq!(cj["mcpServers"]["planetarium"]["url"], "http://127.0.0.1:47615/mcp");
        assert_eq!(cj["mcpServers"]["github"]["url"], "https://example.test/mcp");
        assert_eq!(cj["numStartups"], 41);
        assert!(again["statusLine"]["command"].as_str().unwrap().contains("/planetarium/statusline"));
        assert_eq!(cs::status().status_line, "ours");

        let s = cs::disconnect().unwrap();
        assert!(!s.connected);
        let cj: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
        let orig_cj: Value = serde_json::from_str(claude_json).unwrap();
        assert_eq!(cj, orig_cj, "disconnect leaves ~/.claude.json exactly as it was");

        let restored: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap()).unwrap();
        let orig: Value = serde_json::from_str(original).unwrap();
        assert_eq!(restored, orig, "disconnect leaves exactly the user's own settings");
        // Someone with their own status line keeps it; Planetarium doesn't replace it.
        let theirs = r#"{"statusLine": {"type": "command", "command": "my-status.sh"}}"#;
        std::fs::write(home.join(".claude/settings.json"), theirs).unwrap();
        let s = cs::connect().unwrap();
        assert_eq!(s.status_line, "theirs");
        assert!(!s.outdated, "having your own status line isn't 'outdated'");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(v["statusLine"]["command"], "my-status.sh");
        cs::disconnect().unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(v, serde_json::from_str::<Value>(theirs).unwrap());
    }
}
