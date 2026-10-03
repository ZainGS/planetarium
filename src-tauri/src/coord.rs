//! coord.rs — collision handling: what each agent is told, per the collision mode.
//!
//! Modes (set globally, overridable per repo, see settings.rs):
//!   off   no messages to agents (Planetarium still shows collisions)
//!   warn  the incoming agent gets a note before it touches a held file; nothing waits
//!   ask   the incoming agent's edit pauses until you decide in Planetarium (or the deadline
//!         passes, which falls back to warn)
//!   talk  "Let them work it out": the agents settle it between themselves (talk.rs)
//! Whatever happens, the holder is told on its next tool use that someone else edited its file.

use crate::agents::{display_name, Agent, AgentHub, Collision, HookEvent};
use serde_json::{json, Value};

/// Your decision on an Ask-mode collision.
#[derive(Debug, Clone)]
pub struct Decision {
    /// "proceed" | "wait" | "coordinate"
    pub action: String,
    /// Optional message to the incoming agent.
    pub message: Option<String>,
    /// Optional message to the holder(s), delivered on their next tool use.
    pub note_for_holders: Option<String>,
}

/// What the server should send back for a hook request.
pub enum Reply {
    /// Send this body now (None = empty 200, i.e. "no opinion").
    Now(Option<Value>),
    /// Ask mode: wait for your decision on this collision, falling back to `fallback` at the deadline.
    Ask { collision_id: u64, fallback: Option<Value>, deadline_ms: u64 },
}

/// "Claude · 3f9a (another agent, working on: "fix the shadow flicker")"
fn who(a: &Agent) -> String {
    // Subagents are described by what they were asked to do; main agents by their task.
    let doing = if a.parent_key.is_some() { a.description.as_ref().or(a.task.as_ref()) } else { a.task.as_ref() };
    match doing {
        Some(t) if !t.is_empty() => format!("{} (another agent, working on: \"{}\")", display_name(a), t),
        _ => format!("{} (another agent)", display_name(a)),
    }
}

fn holders_text(hub: &AgentHub, keys: &[String]) -> String {
    let names: Vec<String> = keys.iter().filter_map(|k| hub.get(k)).map(who).collect();
    if names.is_empty() { "Another agent".into() } else { names.join(" and ") }
}

pub fn warn_text(hub: &AgentHub, c: &Collision) -> String {
    let holders = holders_text(hub, &c.holder_keys);
    if c.access == "read" {
        return format!(
            "[Planetarium] Note: {} is changing {} as part of their current task, so it may change again soon. Your copy may be out of date by the time you edit: if you plan to change this file, read it again right before your edit, and do not undo their changes.",
            holders, c.file
        );
    }
    if c.severity == "here" {
        format!(
            "[Planetarium] Heads up: {} is editing {} right now. If this edit fails because the file changed, read it again and re-apply your change on top of theirs. Keep your change as small as possible, and do not undo or overwrite their changes. If your change conflicts with theirs, stop and tell the user.",
            holders, c.file
        )
    } else {
        format!(
            "[Planetarium] Heads up: {} changed {} earlier in their current task and may come back to it. If this edit fails because the file changed, read it again and re-apply your change on top of theirs. Before any further edit to this file, read it again first, and do not undo or overwrite their changes. If your change conflicts with theirs, stop and tell the user.",
            holders, c.file
        )
    }
}

fn context(event: &str, text: String) -> Value {
    json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": text } })
}

fn deny(text: String) -> Value {
    json!({ "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": text
    }})
}

/// Events whose reply can add context for the agent.
fn can_carry_notes(event: &str) -> bool {
    matches!(event, "PreToolUse" | "PostToolUse" | "UserPromptSubmit")
}

/// Messages to hand an agent with this reply. A wind-down message only rides on a tool step
/// (never on a new prompt, where it would be stale).
fn notes_for(hub: &mut AgentHub, key: &str, event: &str, now: u64) -> Vec<String> {
    if !can_carry_notes(event) {
        return Vec::new();
    }
    let mut notes = hub.take_notes(key);
    if matches!(event, "PreToolUse" | "PostToolUse") {
        if let Some(text) = hub.take_wind_note(key, now) {
            notes.insert(0, format!("[Planetarium] Message from the user to all running agents: \"{text}\""));
        }
    }
    notes
}

/// Add queued notes for the agent to a reply (context only; never changes a decision).
fn with_notes(event: &str, reply: Option<Value>, notes: Vec<String>) -> Option<Value> {
    if notes.is_empty() || !can_carry_notes(event) {
        return reply;
    }
    let mut joined = notes.join("\n\n");
    if event == "UserPromptSubmit" {
        // Delivered at the start of a new turn: say these happened while the agent was paused.
        joined = format!("[Planetarium] Since your last turn, while you weren't using tools:\n\n{joined}");
    }
    match reply {
        None => Some(context(event, joined)),
        Some(mut v) => {
            let out = &mut v["hookSpecificOutput"];
            if out.get("permissionDecision").and_then(|d| d.as_str()) == Some("deny") {
                let reason = out["permissionDecisionReason"].as_str().unwrap_or_default().to_string();
                out["permissionDecisionReason"] = json!(format!("{reason}\n\n{joined}"));
            } else {
                let ctx = out["additionalContext"].as_str().unwrap_or_default().to_string();
                out["additionalContext"] = json!(if ctx.is_empty() { joined } else { format!("{ctx}\n\n{joined}") });
            }
            Some(v)
        }
    }
}

/// Decide the reply to a hook event that has already been applied to the hub.
/// `mode_for_repo` gives the collision mode for a repo id; `ask_timeout_ms` is the Ask deadline.
pub fn handle(hub: &mut AgentHub, ev: &HookEvent, repo_file: Option<(String, String)>, mode: &str, ask_timeout_ms: u64, now: u64) -> Reply {
    handle_with_release(hub, ev, repo_file, &[], mode, ask_timeout_ms, now)
}

/// Planetarium's own tools as Claude Code names them.
pub const TOOL_PREFIX: &str = "mcp__planetarium__";

/// Like [`handle`], with the files a `release` call refers to already resolved to
/// (repo, repo-relative path, checkout) by the server.
pub fn handle_with_release(
    hub: &mut AgentHub,
    ev: &HookEvent,
    repo_file: Option<(String, String)>,
    release: &[(String, String, Option<String>)],
    mode: &str,
    ask_timeout_ms: u64,
    now: u64,
) -> Reply {
    let key = ev.agent_key();
    if matches!(ev.event.as_str(), "Stop" | "SubagentStop" | "SessionEnd") {
        crate::talk::turn_ended(hub, &key, now);
    }

    // Planetarium's own tools.
    if let Some(tool) = ev.tool_name.as_deref().and_then(|t| t.strip_prefix(TOOL_PREFIX)) {
        let mut reply = None;
        if ev.event == "PreToolUse" {
            // Remember who's calling, for the tool call that follows.
            if let Some(cid) = ev.collision {
                hub.set_mcp_caller(tool, cid, &key);
            }
        } else if ev.event == "PostToolUse" && tool == "release" {
            let (released, missing) = hub.release(&key, release, ev.release_all);
            let mut parts = Vec::new();
            if !released.is_empty() {
                parts.push(format!("Released {}. Other agents won't be warned about {} any more.", released.join(", "), if released.len() == 1 { "it" } else { "them" }));
            } else if ev.release_all {
                parts.push("You weren't holding any files.".to_string());
            }
            if !missing.is_empty() {
                parts.push(format!("You weren't holding {} (only files you've edited this turn are held).", missing.join(", ")));
            }
            if !parts.is_empty() {
                reply = Some(context("PostToolUse", format!("[Planetarium] {}", parts.join(" "))));
            }
        }
        let notes = notes_for(hub, &key, &ev.event, now);
        return Reply::Now(with_notes(&ev.event, reply, notes));
    }

    let access = ev.tool_name.as_deref().and_then(crate::agents::action_for);
    let mut reply: Option<Value> = None;
    let mut ask: Option<(u64, Option<Value>)> = None;

    if let (Some((repo_id, file)), Some(act)) = (repo_file, access) {
        if !file.is_empty() && act != "search" {
            let holders = hub.holders_of(&key, &repo_id, &file, ev.tree.as_deref());
            let is_edit = act == "edit" || act == "write";

            // After an edit lands, tell the holders (once per editor per turn).
            if ev.event == "PostToolUse" && is_edit && mode != "off" {
                let editor = hub.get(&key).map(who).unwrap_or_else(|| "another agent".into());
                for (h, _) in &holders {
                    if hub.first_notice(h, &file, &key) {
                        hub.queue_note(h, format!(
                            "[Planetarium] {} edited {}, which you had changed earlier. Re-read it before you edit it again, and don't undo their change without checking with the user.",
                            editor, file
                        ));
                    }
                }
            }

            if ev.event == "PreToolUse" && !holders.is_empty() && mode != "off" {
                let here = holders.iter().any(|(_, h)| *h);
                let c = Collision {
                    id: 0,
                    repo_id: repo_id.clone(),
                    file: file.clone(),
                    agent_key: key.clone(),
                    holder_keys: holders.iter().map(|(k, _)| k.clone()).collect(),
                    severity: if here { "here".into() } else { "held".into() },
                    access: if is_edit { "edit".into() } else { "read".into() },
                    mode: if is_edit && mode == "ask" { "ask".into() } else { "warn".into() },
                    status: "warned".into(),
                    created_at: now,
                    resolved_at: Some(now),
                    tree: ev.tree.clone(),
                    ..Default::default()
                };
                let text = warn_text(hub, &c);
                // Once you've approved this agent's edit to this file, don't ask again this turn.
                let ask_mode = is_edit && mode == "ask" && !hub.is_approved(&key, &file);
                let talk_mode = is_edit && mode == "talk" && !hub.is_approved(&key, &file);
                let c = Collision { mode: if ask_mode { "ask".into() } else { "warn".into() }, ..c };
                if talk_mode {
                    let (stop, text) = crate::talk::on_edit(hub, c.clone(), text, now);
                    if !stop {
                        hub.add_collision(c);
                    }
                    reply = Some(if stop { deny(text) } else { context("PreToolUse", text) });
                } else if ask_mode {
                    let deadline = now + ask_timeout_ms;
                    let id = hub.add_collision(Collision { status: "waiting".into(), resolved_at: None, deadline: Some(deadline), ..c });
                    ask = Some((id, Some(context("PreToolUse", text))));
                } else {
                    // Reads only get a note; they aren't listed as collisions.
                    if is_edit {
                        hub.add_collision(c);
                    }
                    reply = Some(context("PreToolUse", text));
                }
            }
        }
    }

    match ask {
        Some((collision_id, fallback)) => {
            // Notes for this agent ride along with whatever the final answer is.
            let notes = notes_for(hub, &key, "PreToolUse", now);
            Reply::Ask { collision_id, fallback: with_notes("PreToolUse", fallback, notes), deadline_ms: ask_timeout_ms }
        }
        None => {
            // Only take messages off the queue when this reply can carry them; otherwise they'd
            // be lost (e.g. on Stop). They wait for the agent's next tool use or next turn.
            let notes = notes_for(hub, &key, &ev.event, now);
            Reply::Now(with_notes(&ev.event, reply, notes))
        }
    }
}

/// Turn your decision into the reply for the paused agent, and queue any note for the holders.
pub fn resolve(hub: &mut AgentHub, collision_id: u64, d: &Decision, fallback: Option<Value>, now: u64) -> Option<Value> {
    let Some(c) = hub.collisions().into_iter().find(|c| c.id == collision_id) else { return fallback };
    let holders = holders_text(hub, &c.holder_keys);
    let extra = d.message.as_deref().map(str::trim).filter(|m| !m.is_empty()).map(|m| format!(" Message from the user: \"{m}\""));
    let extra = extra.unwrap_or_default();

    let (reply, status) = match d.action.as_str() {
        "wait" => (
            deny(format!(
                "[Planetarium] The user asked you not to edit {} yet, because {} is still working on it. Continue with other parts of your task that don't touch this file, and try this edit again later.{}",
                c.file, holders, extra
            )),
            "stopped",
        ),
        "coordinate" => (
            deny(format!(
                "[Planetarium] {} is also working on {}. The user wants you to coordinate before editing it: stop and briefly tell the user what you intend to change in this file and why, then wait for their go-ahead.{}",
                holders, c.file, extra
            )),
            "stopped",
        ),
        _ => (
            context("PreToolUse", format!(
                "[Planetarium] The user approved this edit even though {} is also working on {}. This edit is going ahead now. If it fails because the file changed, read the file again and re-apply your change on top of theirs; before any further edit to this file, read it again first. Don't undo their changes.{}",
                holders, c.file, extra
            )),
            "proceeded",
        ),
    };

    if let Some(note) = d.note_for_holders.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        for h in &c.holder_keys {
            hub.queue_note(h, format!("[Planetarium] Message from the user about {}: \"{}\"", c.file, note));
        }
    }
    let summary = match d.action.as_str() {
        "wait" => "Told to wait",
        "coordinate" => "Told to check with you first",
        _ => "Allowed to proceed",
    };
    let resolution = match d.message.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => format!("{summary}: \"{m}\""),
        None => summary.to_string(),
    };
    if status == "proceeded" {
        hub.approve(&c.agent_key, &c.file);
    }
    hub.update_collision(collision_id, |c| {
        c.status = status.into();
        c.resolved_at = Some(now);
        c.resolution = Some(resolution);
    });
    Some(reply)
}

/// The deadline passed without a decision: fall back to a warning.
pub fn expire(hub: &mut AgentHub, collision_id: u64, now: u64) {
    hub.update_collision(collision_id, |c| {
        if c.status == "waiting" {
            c.status = "expired".into();
            c.resolved_at = Some(now);
            c.resolution = Some("No answer in time; the agent was warned and continued".into());
        }
    });
}

#[cfg(test)]
mod tests {
    use crate::agents::{locate, AgentHub, HookEvent};
    use crate::coord::{self, Decision, Reply};
    use crate::store::RepoMeta;
    use serde_json::Value;

    fn repos() -> Vec<RepoMeta> {
        vec![RepoMeta { id: "salsa".into(), path: "/repos/salsa".into(), name: "salsa".into(), color_index: 0, added_at: 1 }]
    }

    fn ev(event: &str, sid: &str, aid: Option<&str>, tool: Option<&str>, file: Option<&str>) -> HookEvent {
        HookEvent {
            event: event.into(), session_id: sid.into(), agent_id: aid.map(Into::into), agent_type: aid.map(|_| "Explore".into()),
            cwd: Some("/repos/salsa".into()), tool_name: tool.map(Into::into), path: file.map(|f| format!("/repos/salsa/{f}")),
            prompt: if event == "UserPromptSubmit" { Some(format!("task for {sid}")) } else { None }, title: None,
            ..Default::default()
        }
    }

    fn context_of(r: &Reply) -> String {
        match r {
            Reply::Now(Some(v)) => v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string(),
            _ => String::new(),
        }
    }

    #[test]
    fn holder_hears_about_it_on_any_tool_or_its_next_turn() {
        // A edits the file, then B edits it too: A has a message waiting.
        let mut hub = AgentHub::default();
        send(&mut hub, &ev("UserPromptSubmit", "A", None, None, None), "warn", 1);
        send(&mut hub, &ev("PostToolUse", "A", None, Some("Edit"), Some("f.ts")), "warn", 2);
        send(&mut hub, &ev("UserPromptSubmit", "B", None, None, None), "warn", 3);
        send(&mut hub, &ev("PostToolUse", "B", None, Some("Edit"), Some("f.ts")), "warn", 4);
        // A only runs a command next: the message still reaches it.
        let r = send(&mut hub, &ev("PostToolUse", "A", None, Some("Bash"), None), "warn", 5);
        assert!(context_of(&r).contains("edited f.ts"), "{}", context_of(&r));

        // Same story, but A's turn ends first: it hears at the start of its next turn.
        let mut hub = AgentHub::default();
        send(&mut hub, &ev("UserPromptSubmit", "A", None, None, None), "warn", 1);
        send(&mut hub, &ev("PostToolUse", "A", None, Some("Edit"), Some("f.ts")), "warn", 2);
        send(&mut hub, &ev("UserPromptSubmit", "B", None, None, None), "warn", 3);
        send(&mut hub, &ev("PostToolUse", "B", None, Some("Edit"), Some("f.ts")), "warn", 4);
        send(&mut hub, &ev("Stop", "A", None, None, None), "warn", 5);
        let r = send(&mut hub, &ev("UserPromptSubmit", "A", None, None, None), "warn", 6);
        let text = context_of(&r);
        assert!(text.starts_with("[Planetarium] Since your last turn") && text.contains("edited f.ts"), "{text}");
        // Delivered once.
        let r = send(&mut hub, &ev("PostToolUse", "A", None, Some("Bash"), None), "warn", 7);
        assert!(context_of(&r).is_empty());
    }

    #[test]
    fn finished_subagents_messages_go_to_its_parent() {
        let mut hub = AgentHub::default();
        send(&mut hub, &ev("UserPromptSubmit", "A", None, None, None), "warn", 1);
        send(&mut hub, &ev("SubagentStart", "A", Some("s1"), None, None), "warn", 2);
        send(&mut hub, &ev("PostToolUse", "A", Some("s1"), Some("Edit"), Some("f.ts")), "warn", 3);
        send(&mut hub, &ev("UserPromptSubmit", "B", None, None, None), "warn", 4);
        send(&mut hub, &ev("PostToolUse", "B", None, Some("Edit"), Some("f.ts")), "warn", 5);
        send(&mut hub, &ev("SubagentStop", "A", Some("s1"), None, None), "warn", 6);
        let r = send(&mut hub, &ev("PostToolUse", "A", None, Some("Bash"), None), "warn", 7);
        assert!(context_of(&r).contains("edited f.ts"), "{}", context_of(&r));
    }

    fn in_tree(mut e: HookEvent, tree: &str, branch: &str) -> HookEvent {
        e.tree = Some(tree.into());
        e.tree_path = Some(tree.into());
        e.branch = Some(branch.into());
        e
    }

    #[test]
    fn worktrees_dont_collide_but_branch_switches_are_flagged() {
        let mut hub = AgentHub::default();
        // A edits view.ts in the main checkout; B edits the same file in a worktree: no collision.
        send(&mut hub, &in_tree(ev("UserPromptSubmit", "A", None, None, None), "/repos/salsa", "main"), "ask", 1);
        send(&mut hub, &in_tree(ev("PostToolUse", "A", None, Some("Edit"), Some("src/view.ts")), "/repos/salsa", "main"), "ask", 2);
        send(&mut hub, &in_tree(ev("UserPromptSubmit", "B", None, None, None), "/repos/salsa-x", "feature"), "ask", 3);
        let r = send(&mut hub, &in_tree(ev("PreToolUse", "B", None, Some("Edit"), Some("src/view.ts")), "/repos/salsa-x", "feature"), "ask", 4);
        assert!(matches!(r, Reply::Now(None)), "different checkouts never collide");
        assert!(hub.collisions().is_empty());
        assert_eq!(hub.get("B").unwrap().branch.as_deref(), Some("feature"));

        // C in the main checkout does collide with A.
        send(&mut hub, &in_tree(ev("UserPromptSubmit", "C", None, None, None), "/repos/salsa", "main"), "ask", 5);
        let r = send(&mut hub, &in_tree(ev("PreToolUse", "C", None, Some("Edit"), Some("src/view.ts")), "/repos/salsa", "main"), "ask", 6);
        assert!(matches!(r, Reply::Ask { .. }), "same checkout still collides");

        // Someone switches the main checkout's branch: A and C are flagged and told; B isn't.
        assert!(hub.observe_branch("/repos/salsa", "experiment", 7));
        assert_eq!(hub.get("A").unwrap().branch_switch.as_ref().map(|b| (b.from.as_str(), b.to.as_str())), Some(("main", "experiment")));
        assert!(hub.get("B").unwrap().branch_switch.is_none());
        let r = send(&mut hub, &in_tree(ev("PostToolUse", "A", None, Some("Bash"), None), "/repos/salsa", "experiment"), "ask", 8);
        assert!(context_of(&r).contains("changed from main to experiment"), "{}", context_of(&r));
        // Seen once; the next event on the same branch doesn't re-flag.
        assert!(!hub.observe_branch("/repos/salsa", "experiment", 9));
    }

    fn deny_of(r: &Reply) -> Option<String> {
        match r {
            Reply::Now(Some(v)) if v["hookSpecificOutput"]["permissionDecision"] == "deny" => {
                v["hookSpecificOutput"]["permissionDecisionReason"].as_str().map(String::from)
            }
            _ => None,
        }
    }

    fn collision_number(text: &str) -> u64 {
        let i = text.find("collision ").expect("mentions a collision") + "collision ".len();
        text[i..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap()
    }

    /// A edits f.ts; B starts a turn.
    fn a_holds_f(hub: &mut AgentHub, mode: &str) {
        send(hub, &ev("UserPromptSubmit", "A", None, None, None), mode, 1);
        send(hub, &ev("PostToolUse", "A", None, Some("Edit"), Some("f.ts")), mode, 2);
        send(hub, &ev("UserPromptSubmit", "B", None, None, None), mode, 3);
    }

    #[test]
    fn release_drops_holds_early() {
        let mut hub = AgentHub::default();
        a_holds_f(&mut hub, "warn");
        let mut rel = ev("PostToolUse", "A", None, Some("mcp__planetarium__release"), None);
        rel.release_files = vec!["/repos/salsa/f.ts".into(), "/repos/salsa/g.ts".into()];
        hub.apply(&rel, &repos(), 4);
        let items = vec![("salsa".to_string(), "f.ts".to_string(), None), ("salsa".to_string(), "g.ts".to_string(), None)];
        let r = coord::handle_with_release(&mut hub, &rel, None, &items, "warn", 300_000, 4);
        let t = context_of(&r);
        assert!(t.contains("Released f.ts") && t.contains("weren't holding g.ts"), "{t}");
        let r = send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "warn", 5);
        assert!(matches!(r, Reply::Now(None)), "no warning once released");
    }

    #[test]
    fn talk_mode_full_exchange() {
        use crate::talk::{self, Sent};
        let mut hub = AgentHub::default();
        a_holds_f(&mut hub, "talk");

        // B's edit is paused with a collision number.
        let first = deny_of(&send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "talk", 4)).expect("paused");
        println!("PAUSED: {first}");
        let id = collision_number(&first);
        assert!(first.contains("message_agent"));
        // Trying again without messaging: still paused, with a reminder.
        let again = deny_of(&send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "talk", 5)).unwrap();
        assert!(again.contains("First send"), "{again}");

        // Roles are checked.
        assert!(talk::message(&mut hub, id, Some("A"), "hi", 6).is_err());
        assert!(talk::reply(&mut hub, id, Some("B"), "go_ahead", "", 6).is_err());

        // B writes; A hears about it on its next step and asks B to wait.
        assert!(matches!(talk::message(&mut hub, id, Some("B"), "I need to rename the export in f.ts", 7), Ok(Sent::Wait)));
        let note = context_of(&send(&mut hub, &ev("PostToolUse", "A", None, Some("Read"), Some("g.ts")), "talk", 8));
        println!("TO A: {note}");
        assert!(note.contains("rename the export") && note.contains(&format!("collision {id}")) && note.contains("reply_to_agent"));
        let (to_b, ack) = talk::reply(&mut hub, id, Some("A"), "wait", "two more minutes, mid-refactor", 9).unwrap();
        println!("TO B: {to_b}\nACK: {ack}");
        assert!(to_b.contains("asked you to wait") && to_b.contains("mid-refactor"));
        let waiting = deny_of(&send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "talk", 10)).unwrap();
        assert!(waiting.contains("asked you to wait") && waiting.contains("2 messages left"), "{waiting}");

        // B asks again; A is done, which releases the file and lets B through.
        assert!(matches!(talk::message(&mut hub, id, Some("B"), "ready now?", 11), Ok(Sent::Wait)));
        let (to_b, _) = talk::reply(&mut hub, id, Some("A"), "done", "all yours", 12).unwrap();
        assert!(to_b.contains("has finished with f.ts"), "{to_b}");
        assert!(hub.get("A").unwrap().holds.is_empty());
        assert_eq!(hub.collision(id).unwrap().status, "agreed");
        assert_eq!(hub.collision(id).unwrap().messages.len(), 4);
        assert!(deny_of(&send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "talk", 13)).is_none());
    }

    #[test]
    fn talk_mode_limits_and_fallbacks() {
        use crate::talk::{self, Sent, MAX_ROUNDS, MAX_SILENT_ATTEMPTS};
        // An agent that never uses the tools is let through with a warning, not stuck forever.
        let mut hub = AgentHub::default();
        a_holds_f(&mut hub, "talk");
        for n in 0..MAX_SILENT_ATTEMPTS {
            assert!(deny_of(&send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "talk", 4 + n as u64)).is_some());
        }
        let r = send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "talk", 10);
        assert!(deny_of(&r).is_none() && context_of(&r).contains("Heads up"));
        assert!(hub.collisions().iter().any(|c| c.status == "no_tools"));

        // Rounds run out → it needs you.
        let mut hub = AgentHub::default();
        a_holds_f(&mut hub, "talk");
        let id = collision_number(&deny_of(&send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "talk", 4)).unwrap());
        for n in 0..MAX_ROUNDS {
            assert!(matches!(talk::message(&mut hub, id, Some("B"), "please?", 5 + n as u64), Ok(Sent::Wait)));
            talk::reply(&mut hub, id, Some("A"), "wait", "not yet", 6 + n as u64).unwrap();
        }
        assert!(talk::message(&mut hub, id, Some("B"), "please??", 20).is_err());
        let t = deny_of(&send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "talk", 21)).unwrap();
        assert!(t.contains("ask the user"), "{t}");
        // You step in.
        let t = talk::user_decision(&mut hub, id, "proceed", Some("fine, go"), 22).unwrap();
        assert!(t.contains("go ahead") && t.contains("fine, go"));
        assert!(deny_of(&send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "talk", 23)).is_none());

        // No reply in time → go ahead carefully.
        let mut hub = AgentHub::default();
        a_holds_f(&mut hub, "talk");
        let id = collision_number(&deny_of(&send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "talk", 4)).unwrap());
        talk::message(&mut hub, id, Some("B"), "may I?", 5).unwrap();
        assert!(talk::timeout(&mut hub, id, 6).unwrap().contains("No answer"));
        assert!(talk::timeout(&mut hub, id, 7).is_none(), "only once");

        // The holder's turn ends mid-exchange → the waiting agent is told to go ahead.
        let mut hub = AgentHub::default();
        a_holds_f(&mut hub, "talk");
        let id = collision_number(&deny_of(&send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "talk", 4)).unwrap());
        talk::message(&mut hub, id, Some("B"), "may I?", 5).unwrap();
        send(&mut hub, &ev("Stop", "A", None, None, None), "talk", 6);
        let settled = talk::recheck(&mut hub, 7);
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].1, "B");
        assert!(settled[0].2.contains("finished with f.ts"));

        // The paused agent's own turn ends → the exchange closes.
        let mut hub = AgentHub::default();
        a_holds_f(&mut hub, "talk");
        let id = collision_number(&deny_of(&send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("f.ts")), "talk", 4)).unwrap());
        send(&mut hub, &ev("Stop", "B", None, None, None), "talk", 5);
        assert_eq!(hub.collision(id).unwrap().status, "closed");
    }

    #[test]
    fn wind_down_reaches_agents_mid_turn_only() {
        let mut hub = AgentHub::default();
        send(&mut hub, &ev("UserPromptSubmit", "A", None, None, None), "warn", 1);
        send(&mut hub, &ev("PostToolUse", "A", None, Some("Read"), Some("a.ts")), "warn", 2);
        send(&mut hub, &ev("UserPromptSubmit", "B", None, None, None), "warn", 3);
        send(&mut hub, &ev("Stop", "B", None, None, None), "warn", 4);
        assert_eq!(hub.wind_down("find a stopping point", 5), 1, "only A is mid-turn");
        assert_eq!(hub.get("A").unwrap().wind_down.as_deref(), Some("asked"));
        // Not on a new prompt…
        let r = send(&mut hub, &ev("PreToolUse", "A", None, Some("Read"), Some("b.ts")), "warn", 6);
        let t = context_of(&r);
        assert!(t.contains("find a stopping point"), "{t}");
        assert_eq!(hub.get("A").unwrap().wind_down.as_deref(), Some("told"));
        // …and only once.
        assert!(context_of(&send(&mut hub, &ev("PreToolUse", "A", None, Some("Read"), Some("c.ts")), "warn", 7)).is_empty());
        send(&mut hub, &ev("Stop", "A", None, None, None), "warn", 8);
        assert_eq!(hub.get("A").unwrap().wind_down.as_deref(), Some("stopped"));
        // You tell it to continue: cleared.
        send(&mut hub, &ev("UserPromptSubmit", "A", None, None, None), "warn", 9);
        assert_eq!(hub.get("A").unwrap().wind_down, None);
        // A message that never got delivered isn't handed over at the next turn.
        let mut hub = AgentHub::default();
        send(&mut hub, &ev("UserPromptSubmit", "A", None, None, None), "warn", 1);
        hub.wind_down("stop", 2);
        send(&mut hub, &ev("Stop", "A", None, None, None), "warn", 3);
        let r = send(&mut hub, &ev("UserPromptSubmit", "A", None, None, None), "warn", 4);
        assert!(context_of(&r).is_empty());
        assert_eq!(hub.get("A").unwrap().wind_down, None);
    }

    /// Apply + handle like server.rs does.
    fn send(hub: &mut AgentHub, e: &HookEvent, mode: &str, now: u64) -> Reply {
        let r = repos();
        let rf = e.path.as_deref().and_then(|p| locate(&r, p));
        hub.apply(e, &r, now);
        coord::handle(hub, e, rf, mode, 300_000, now)
    }

    fn ctx(r: &Reply) -> Option<String> {
        match r { Reply::Now(Some(v)) => v["hookSpecificOutput"]["additionalContext"].as_str().map(String::from), _ => None }
    }

    #[test]
    fn warn_mode_full_story() {
        let mut hub = AgentHub::default();
        send(&mut hub, &ev("UserPromptSubmit", "A", None, None, None), "warn", 1);
        send(&mut hub, &ev("UserPromptSubmit", "B", None, None, None), "warn", 1);
        // A edits view.ts (hold), then moves on to main.ts.
        assert!(matches!(send(&mut hub, &ev("PreToolUse", "A", None, Some("Edit"), Some("src/view.ts")), "warn", 2), Reply::Now(None)));
        send(&mut hub, &ev("PostToolUse", "A", None, Some("Edit"), Some("src/view.ts")), "warn", 3);
        send(&mut hub, &ev("PostToolUse", "A", None, Some("Read"), Some("src/main.ts")), "warn", 4);

        // B reads view.ts: gets a note, no collision listed.
        let r = send(&mut hub, &ev("PreToolUse", "B", None, Some("Read"), Some("src/view.ts")), "warn", 5);
        let t = ctx(&r).expect("read note");
        println!("READ NOTE: {t}");
        assert!(t.contains("may change again"), "{t}");
        assert!(hub.collisions().is_empty());

        // B is about to edit view.ts: warned (held, not here), collision listed.
        let r = send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("src/view.ts")), "warn", 6);
        let t = ctx(&r).expect("edit warning");
        println!("EDIT WARNING: {t}");
        assert!(t.contains("earlier in their current task") && t.contains("task for A"));
        assert_eq!(hub.collisions().len(), 1);
        assert_eq!(hub.collisions()[0].severity, "held");

        // B's edit lands: A gets told on its next tool use, exactly once.
        send(&mut hub, &ev("PostToolUse", "B", None, Some("Edit"), Some("src/view.ts")), "warn", 7);
        send(&mut hub, &ev("PostToolUse", "B", None, Some("Edit"), Some("src/view.ts")), "warn", 8);
        let r = send(&mut hub, &ev("PreToolUse", "A", None, Some("Read"), Some("src/other.ts")), "warn", 9);
        let t = ctx(&r).expect("note for A");
        println!("NOTE FOR A: {t}");
        assert!(t.contains("edited src/view.ts"));
        assert_eq!(t.matches("[Planetarium]").count(), 1);
        assert!(ctx(&send(&mut hub, &ev("PreToolUse", "A", None, Some("Read"), Some("src/other.ts")), "warn", 10)).is_none());

        // A's turn ends → its hold is gone → B can edit freely.
        send(&mut hub, &ev("Stop", "A", None, None, None), "warn", 11);
        assert!(matches!(send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("src/view.ts")), "warn", 12), Reply::Now(None)));
    }

    #[test]
    fn here_now_is_stronger() {
        let mut hub = AgentHub::default();
        send(&mut hub, &ev("PostToolUse", "A", None, Some("Edit"), Some("src/view.ts")), "warn", 1);
        send(&mut hub, &ev("PreToolUse", "A", None, Some("Edit"), Some("src/view.ts")), "warn", 2);
        let t = ctx(&send(&mut hub, &ev("PreToolUse", "B", None, Some("Write"), Some("src/view.ts")), "warn", 3)).unwrap();
        assert!(t.contains("right now"), "{t}");
    }

    #[test]
    fn off_mode_says_nothing() {
        let mut hub = AgentHub::default();
        send(&mut hub, &ev("PostToolUse", "A", None, Some("Edit"), Some("src/view.ts")), "off", 1);
        assert!(matches!(send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("src/view.ts")), "off", 2), Reply::Now(None)));
        send(&mut hub, &ev("PostToolUse", "B", None, Some("Edit"), Some("src/view.ts")), "off", 3);
        assert!(matches!(send(&mut hub, &ev("PreToolUse", "A", None, Some("Read"), Some("x.ts")), "off", 4), Reply::Now(None)));
    }

    #[test]
    fn ask_mode_decisions() {
        for (action, expect_deny) in [("wait", true), ("coordinate", true), ("proceed", false)] {
            let mut hub = AgentHub::default();
            send(&mut hub, &ev("PostToolUse", "A", None, Some("Edit"), Some("src/view.ts")), "ask", 1);
            let r = send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("src/view.ts")), "ask", 2);
            let (id, fallback) = match r { Reply::Ask { collision_id, fallback, .. } => (collision_id, fallback), _ => panic!("expected ask") };
            assert_eq!(hub.collisions()[0].status, "waiting");
            let d = Decision { action: action.into(), message: Some("keep render() as is".into()), note_for_holders: Some("B will touch view.ts next".into()) };
            let reply: Value = coord::resolve(&mut hub, id, &d, fallback, 3).unwrap();
            let out = &reply["hookSpecificOutput"];
            println!("{action}: {}", reply);
            assert_eq!(out.get("permissionDecision").and_then(|v| v.as_str()) == Some("deny"), expect_deny);
            assert_ne!(out.get("permissionDecision").and_then(|v| v.as_str()), Some("allow"), "never auto-approve");
            assert!(reply.to_string().contains("keep render() as is"));
            // Holder gets the user's note on its next tool use.
            let t = ctx(&send(&mut hub, &ev("PreToolUse", "A", None, Some("Read"), Some("y.ts")), "ask", 4)).unwrap();
            assert!(t.contains("B will touch view.ts next"));
            // After "proceed", B isn't asked again for the same file this turn (only warned).
            let again = send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("src/view.ts")), "ask", 5);
            assert_eq!(matches!(again, Reply::Ask { .. }), action != "proceed");
        }
    }

    #[test]
    fn ask_mode_timeout_falls_back_to_warning() {
        let mut hub = AgentHub::default();
        send(&mut hub, &ev("PostToolUse", "A", None, Some("Edit"), Some("src/view.ts")), "ask", 1);
        let r = send(&mut hub, &ev("PreToolUse", "B", None, Some("Edit"), Some("src/view.ts")), "ask", 2);
        let (id, fallback) = match r { Reply::Ask { collision_id, fallback, .. } => (collision_id, fallback), _ => panic!() };
        coord::expire(&mut hub, id, 3);
        assert_eq!(hub.collisions()[0].status, "expired");
        assert!(fallback.unwrap().to_string().contains("additionalContext"));
    }

    #[test]
    fn subagent_working_for_its_parent_is_not_a_collision() {
        let mut hub = AgentHub::default();
        send(&mut hub, &ev("PostToolUse", "S", None, Some("Edit"), Some("src/view.ts")), "ask", 1);
        assert!(matches!(send(&mut hub, &ev("PreToolUse", "S", Some("x"), Some("Edit"), Some("src/view.ts")), "ask", 2), Reply::Now(None)));
    }

}
