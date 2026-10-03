//! talk.rs — "Let them work it out": agents settle a collision between themselves.
//!
//! When an agent (the *requester*) is about to edit a file another agent is holding, its edit is
//! paused (denied, with an explanation) and it's given a collision number. It then:
//!   1. sends the holder a message with Planetarium's `message_agent` tool, which waits for the
//!      answer (up to REPLY_WAIT_MS);
//!   2. the holder sees the message on its next step and answers with `reply_to_agent`:
//!      go_ahead, wait (with a reason), or done (it's finished with the file, which releases it);
//!   3. the answer comes back as the result of step 1. After go_ahead / done, the edit goes
//!      through; after wait, it's paused again until they agree, rounds run out, or you decide.
//!
//! Limits, so two agents can never loop forever or block each other:
//!   - at most MAX_ROUNDS messages from the requester per collision, then it needs you;
//!   - no reply within REPLY_WAIT_MS → the requester may go ahead carefully (like Warn);
//!   - if the requester keeps retrying without ever messaging (e.g. its session started before
//!     Planetarium's tools were installed), it's let through with a warning after
//!     MAX_SILENT_ATTEMPTS tries;
//!   - when the holder's turn ends or it releases the file, the requester is told to go ahead;
//!   - you can step in from Planetarium at any time.
//!
//! Everything here is plain state changes; server.rs delivers the texts.

use crate::agents::{display_name, AgentHub, Collision, TalkMessage};

pub const MAX_ROUNDS: u32 = 3;
pub const REPLY_WAIT_MS: u64 = 90_000;
pub const MAX_SILENT_ATTEMPTS: u32 = 3;

pub fn is_open(c: &Collision) -> bool {
    c.mode == "talk" && matches!(c.status.as_str(), "talking" | "asked_to_wait" | "needs_you")
}

fn name(hub: &AgentHub, key: &str) -> String {
    hub.get(key).map(display_name).unwrap_or_else(|| "The other agent".into())
}

fn names(hub: &AgentHub, keys: &[String]) -> String {
    let v: Vec<String> = keys.iter().map(|k| name(hub, k)).collect();
    if v.is_empty() { "the other agent".into() } else { v.join(" and ") }
}

fn task_of(hub: &AgentHub, key: &str) -> String {
    let doing = hub.get(key).and_then(|a| if a.parent_key.is_some() { a.description.clone().or(a.task.clone()) } else { a.task.clone() });
    match doing {
        Some(t) if !t.is_empty() => format!(" (working on: \"{t}\")"),
        _ => String::new(),
    }
}

/// The open talk collision for this agent and file, if any.
fn open_for(hub: &AgentHub, key: &str, repo_id: &str, file: &str) -> Option<Collision> {
    hub.collisions()
        .into_iter()
        .find(|c| is_open(c) && c.agent_key == key && c.repo_id == repo_id && c.file.eq_ignore_ascii_case(file))
}

/// What the paused agent sees when its edit is held up. Returns (deny?, text): deny=false means
/// the edit may go ahead with `text` as a warning.
pub fn on_edit(hub: &mut AgentHub, template: Collision, warn: String, now: u64) -> (bool, String) {
    let key = template.agent_key.clone();
    let Some(c) = open_for(hub, &key, &template.repo_id, &template.file) else {
        let holders = names(hub, &template.holder_keys);
        let tasks: String = template.holder_keys.iter().map(|k| task_of(hub, k)).collect();
        let doing = if template.severity == "here" { "is editing it right now" } else { "changed it earlier in their current task and may come back to it" };
        let id = hub.add_collision(Collision {
            mode: "talk".into(),
            status: "talking".into(),
            resolved_at: None,
            attempts: 1,
            ..template.clone()
        });
        return (true, format!(
            "[Planetarium] This edit to {file} is paused: {holders}{tasks} {doing}. The user has asked agents to work this out between themselves. Use the planetarium tool message_agent with collision {id} to tell them, in a sentence or two, what you want to change in {file} and why. It waits for their answer. If they agree, try this edit again. Until then, carry on with other parts of your task that don't touch this file.",
            file = template.file
        ));
    };

    let id = c.id;
    let attempts = c.attempts + 1;
    hub.update_collision(id, |c| c.attempts = attempts);
    let holders = names(hub, &c.holder_keys);
    match c.status.as_str() {
        "needs_you" => (true, format!(
            "[Planetarium] Collision {id} on {} needs the user's decision now. Stop and ask the user how to proceed with this file.",
            c.file
        )),
        "asked_to_wait" => {
            let last = c.messages.iter().rev().find(|m| m.from != key).map(|m| m.text.clone()).unwrap_or_default();
            let left = MAX_ROUNDS.saturating_sub(c.rounds);
            if left == 0 {
                set_status(hub, id, "needs_you", None, now);
                (true, format!("[Planetarium] {holders} asked you to wait on {} (\"{last}\"), and you've used all {MAX_ROUNDS} messages for collision {id}. Stop and ask the user how to proceed with this file.", c.file))
            } else {
                (true, format!("[Planetarium] {holders} asked you to wait before editing {}: \"{last}\". Work on something else for now, or message them again with message_agent (collision {id}, {left} message{} left).", c.file, if left == 1 { "" } else { "s" }))
            }
        }
        _ if c.rounds == 0 && attempts > MAX_SILENT_ATTEMPTS => {
            // It never used the tools: most likely its session doesn't have them. Don't deadlock.
            set_status(hub, id, "no_tools", Some("The agent didn't use Planetarium's messaging tools (its session may need a restart to get them); it was warned and continued.".into()), now);
            hub.approve(&key, &c.file);
            (false, warn)
        }
        _ if c.rounds == 0 => (true, format!(
            "[Planetarium] {} is still paused (collision {id}). First send {holders} a short message with the planetarium tool message_agent (collision {id}) saying what you want to change. If you don't have that tool, tell the user.",
            c.file
        )),
        _ => (true, format!(
            "[Planetarium] Still waiting for {holders} to answer about {} (collision {id}). Carry on with other parts of your task, then try again.",
            c.file
        )),
    }
}

fn set_status(hub: &mut AgentHub, id: u64, status: &str, resolution: Option<String>, now: u64) {
    let open = matches!(status, "talking" | "asked_to_wait" | "needs_you");
    hub.update_collision(id, |c| {
        c.status = status.into();
        if !open {
            c.resolved_at = Some(now);
        }
        if resolution.is_some() {
            c.resolution = resolution;
        }
    });
}

pub enum Sent {
    /// Wait for the holder's reply (deliver with server's waiter, or REPLY_WAIT_MS timeout).
    Wait,
    /// Answer straight away with this text.
    Now(String),
}

/// The paused agent's `message_agent` call. `caller` is who made it, if Planetarium knows.
pub fn message(hub: &mut AgentHub, id: u64, caller: Option<&str>, text: &str, now: u64) -> Result<Sent, String> {
    let Some(c) = hub.collision(id).cloned() else {
        return Err(format!("There's no collision {id}. Use the number Planetarium gave you when it paused your edit."));
    };
    if c.mode != "talk" {
        return Err(format!("Collision {id} isn't one agents settle between themselves. Follow the instructions Planetarium gave you."));
    }
    if let Some(k) = caller {
        if k != c.agent_key {
            return Err(format!("message_agent is for the agent whose edit was paused. If another agent messaged you about collision {id}, answer with reply_to_agent."));
        }
    }
    let text = text.trim();
    if text.is_empty() {
        return Err("Say what you want to change and why.".into());
    }
    match c.status.as_str() {
        "talking" | "asked_to_wait" => {}
        "needs_you" => return Err(format!("Collision {id} needs the user's decision now. Ask the user how to proceed.")),
        "agreed" | "no_reply" | "no_tools" => return Ok(Sent::Now(format!("You can already go ahead with your edit to {}.", c.file))),
        _ => return Err(format!("Collision {id} is closed.")),
    }
    if c.rounds >= MAX_ROUNDS {
        set_status(hub, id, "needs_you", None, now);
        return Err(format!("You've used all {MAX_ROUNDS} messages for collision {id}. Stop and ask the user how to proceed with {}.", c.file));
    }
    // Did the holders finish with the file in the meantime?
    if hub.holders_of(&c.agent_key, &c.repo_id, &c.file, c.tree.as_deref()).is_empty() {
        set_status(hub, id, "agreed", Some("The other agent finished with the file".into()), now);
        hub.approve(&c.agent_key, &c.file);
        return Ok(Sent::Now(format!("{} is no longer held by anyone. Go ahead with your edit: read it again first.", c.file)));
    }

    let from = name(hub, &c.agent_key);
    let task = task_of(hub, &c.agent_key);
    let rounds = c.rounds + 1;
    hub.update_collision(id, |c| {
        c.rounds = rounds;
        c.status = "talking".into();
        c.messages.push(TalkMessage { from: c.agent_key.clone(), text: text.to_string(), decision: None, at: now });
    });
    for h in &c.holder_keys {
        hub.queue_note(h, format!(
            "[Planetarium] {from}{task} wants to edit {file}, which you changed earlier in your task (collision {id}). They say: \"{text}\". Answer now with the planetarium tool reply_to_agent (collision {id}): decision \"go_ahead\" if their change is fine, \"wait\" if you still need the file (say why and roughly for how long), or \"done\" if you've finished with {file} (that releases it). Add a short message. They're paused on this file until you answer.",
            file = c.file
        ));
    }
    Ok(Sent::Wait)
}

/// A holder's `reply_to_agent` call. Returns (text for the paused agent, acknowledgement for
/// the holder).
pub fn reply(hub: &mut AgentHub, id: u64, caller: Option<&str>, decision: &str, text: &str, now: u64) -> Result<(String, String), String> {
    let Some(c) = hub.collision(id).cloned() else {
        return Err(format!("There's no collision {id}."));
    };
    if c.mode != "talk" || !is_open(&c) {
        return Err(format!("Collision {id} is already settled; nothing to answer."));
    }
    let replier = match caller {
        Some(k) if c.holder_keys.iter().any(|h| h == k) => k.to_string(),
        Some(k) if k == c.agent_key => return Err(format!("You're the agent waiting on collision {id}; use message_agent to write to them.")),
        Some(_) => return Err(format!("Only the agent(s) holding {} can answer collision {id}.", c.file)),
        None => c.holder_keys.first().cloned().unwrap_or_default(),
    };
    let decision = match decision.trim().to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
        "go_ahead" | "goahead" | "yes" | "ok" | "agree" => "go_ahead",
        "wait" | "no" | "hold" => "wait",
        "done" | "release" | "finished" => "done",
        other => return Err(format!("decision must be \"go_ahead\", \"wait\" or \"done\" (got \"{other}\").")),
    };
    let text = text.trim().to_string();
    let who = name(hub, &replier);
    let said = if text.is_empty() { String::new() } else { format!(": \"{text}\"") };
    hub.update_collision(id, |c| c.messages.push(TalkMessage { from: replier.clone(), text: text.clone(), decision: Some(decision.into()), at: now }));

    let file = c.file.clone();
    let out = match decision {
        "wait" => {
            set_status(hub, id, "asked_to_wait", None, now);
            let left = MAX_ROUNDS.saturating_sub(c.rounds);
            let more = if left > 0 { format!(" You can message them again later (collision {id}, {left} left).") } else { " If you can't continue without it, ask the user.".into() };
            (format!("[Planetarium] {who} asked you to wait before editing {file}{said}. Work on other parts of your task for now.{more}"),
             format!("Sent. They'll hold off on {file}. Release it when you're done (planetarium release tool), or it's released when your turn ends."))
        }
        _ => {
            if decision == "done" {
                if let Some(a) = hub.get(&replier) {
                    let held: Vec<_> = a.holds.iter().filter(|h| h.repo_id == c.repo_id && h.file.eq_ignore_ascii_case(&file)).map(|h| (h.repo_id.clone(), h.file.clone(), h.tree.clone())).collect();
                    hub.release(&replier, &held, false);
                }
            }
            hub.approve(&c.agent_key, &file);
            set_status(hub, id, "agreed", Some(format!("{who}: {}", if decision == "done" { "done with the file" } else { "go ahead" })), now);
            let verb = if decision == "done" { "has finished with" } else { "says you can go ahead with" };
            (format!("[Planetarium] {who} {verb} {file}{said}. Go ahead with your edit: read {file} again first so you build on their changes, and don't undo them."),
             if decision == "done" { format!("Sent, and {file} is released.") } else { format!("Sent. They'll edit {file} now; re-read it before you change it again.") })
        }
    };
    Ok(out)
}

/// No reply came in time. Returns the text for the paused agent, or None if things moved on.
pub fn timeout(hub: &mut AgentHub, id: u64, now: u64) -> Option<String> {
    let c = hub.collision(id).cloned()?;
    if c.status != "talking" {
        return None;
    }
    let holders = names(hub, &c.holder_keys);
    set_status(hub, id, "no_reply", Some("No reply in time; the agent was told to go ahead carefully".into()), now);
    hub.approve(&c.agent_key, &c.file);
    Some(format!(
        "[Planetarium] No answer from {holders} within {} seconds (they may be in the middle of a long step). You may go ahead: read {} again right before editing, keep your change small, and don't undo their changes. If the change is risky, ask the user instead.",
        REPLY_WAIT_MS / 1000, c.file
    ))
}

/// You stepped in from Planetarium. action: "proceed" | "wait". Returns text for the paused agent.
pub fn user_decision(hub: &mut AgentHub, id: u64, action: &str, message: Option<&str>, now: u64) -> Option<String> {
    let c = hub.collision(id).cloned()?;
    if !is_open(&c) {
        return None;
    }
    let msg = message.map(str::trim).filter(|m| !m.is_empty());
    let extra = msg.map(|m| format!(" Message from the user: \"{m}\"")).unwrap_or_default();
    hub.update_collision(id, |c| {
        if let Some(m) = msg {
            c.messages.push(TalkMessage { from: "user".into(), text: m.to_string(), decision: Some(action.into()), at: now });
        }
    });
    if action == "wait" {
        set_status(hub, id, "closed", Some("You told it to wait".into()), now);
        Some(format!("[Planetarium] The user decided you should not edit {} for now; leave it to the other agent and carry on with other work.{extra}", c.file))
    } else {
        hub.approve(&c.agent_key, &c.file);
        set_status(hub, id, "agreed", Some("You let it proceed".into()), now);
        Some(format!("[Planetarium] The user says you can go ahead with your edit to {}. Read it again first and don't undo the other agent's changes.{extra}", c.file))
    }
}

/// Housekeeping after turns end, files are released, or agents disappear: open exchanges whose
/// file is no longer held are settled (texts returned for the paused agents), and ones whose
/// paused agent is gone are closed.
pub fn recheck(hub: &mut AgentHub, now: u64) -> Vec<(u64, String, String)> {
    let mut out = Vec::new();
    for c in hub.collisions().into_iter().filter(is_open) {
        // (Not "idle": an agent waiting inside message_agent sends no events for a while.)
        let requester_live = hub.get(&c.agent_key).map_or(false, |a| a.status != "done");
        if !requester_live {
            set_status(hub, c.id, "closed", Some("The paused agent finished its turn".into()), now);
            continue;
        }
        if hub.holders_of(&c.agent_key, &c.repo_id, &c.file, c.tree.as_deref()).is_empty() {
            hub.approve(&c.agent_key, &c.file);
            set_status(hub, c.id, "agreed", Some("The other agent finished with the file".into()), now);
            out.push((c.id, c.agent_key.clone(), format!(
                "[Planetarium] {} has finished with {} (it's no longer held). Go ahead with your edit: read it again first so you build on their changes.",
                names(hub, &c.holder_keys), c.file
            )));
        }
    }
    out
}

/// The paused agent's turn ended: its open exchanges are closed (it'll start fresh next turn).
pub fn turn_ended(hub: &mut AgentHub, key: &str, now: u64) {
    for c in hub.collisions().into_iter().filter(|c| is_open(c) && c.agent_key == key) {
        set_status(hub, c.id, "closed", Some("The paused agent finished its turn".into()), now);
    }
}
