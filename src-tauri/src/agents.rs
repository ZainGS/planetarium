//! agents.rs — the live picture of which coding agents are where, what they're holding, and
//! where they collide.
//!
//! Claude Code hook events (see server.rs) are turned into [`HookEvent`]s and applied here.
//! Each Claude Code session is one "main" agent; each subagent it spawns is its own agent
//! linked to that parent. An agent is matched to one of your repos by the file it touches
//! (or, before it touches anything, by the folder Claude Code is running in).
//!
//! Status, as the page shows it:
//!   working  touched a file recently            → flies to that file's star
//!   idle     finished its turn / quiet a while  → orbits the repo's ring
//!   waiting  idle main agent with subagents out → orbits the ring, tethered to them
//!   done     session or subagent ended          → fades out, then is dropped
//!
//! Holds: every file an agent edits is automatically "held" by it until its turn ends (main
//! agent: Stop; subagent: SubagentStop; anyone: SessionEnd). A held file is one the agent may
//! still come back to. Another agent touching a held file is a collision (see coord.rs).
//!
//! Checkouts and branches (git.rs): an agent in a git worktree is shown on the matching file of
//! the repo you added, with its branch. Holds belong to one checkout, so agents in different
//! worktrees never collide. If the branch checked out in a folder changes under agents working
//! there (someone ran `git checkout`), they're flagged and told.

use crate::store::RepoMeta;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};

/// No file activity for this long → the agent drifts out to the ring (its holds stay).
pub const IDLE_AFTER_MS: u64 = 20_000;
/// Finished agents linger this long so the page can fade them out.
pub const DONE_LINGER_MS: u64 = 8_000;
/// Agents with no events at all for this long are forgotten (session closed without SessionEnd).
pub const FORGET_AFTER_MS: u64 = 30 * 60_000;
/// Resolved / warned collisions stay listed this long.
pub const COLLISION_LINGER_MS: u64 = 45_000;

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Hold {
    pub repo_id: String,
    pub file: String,
    pub since: u64,
    /// Which checkout the edit happened in (git.rs tree_key); None outside git.
    #[serde(skip)]
    pub tree: Option<String>,
}

/// The branch checked out in an agent's folder changed while it was working there.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BranchSwitch {
    pub from: String,
    pub to: String,
    pub at: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    /// session id, plus ":" + agent id for subagents.
    pub key: String,
    pub session_id: String,
    pub agent_id: Option<String>,
    /// "main" for the session's own agent, else the subagent type (Explore, Plan, …).
    pub agent_type: String,
    pub parent_key: Option<String>,
    pub repo_id: Option<String>,
    /// Repo-relative path, with forward slashes.
    pub file: Option<String>,
    /// read | edit | write | search
    pub action: Option<String>,
    pub status: String,
    /// The latest prompt given to the session (main agents), shortened.
    pub task: Option<String>,
    /// Subagents: the short description it was started with ("Fix blend mode bug"), and the
    /// instructions it was given (shortened).
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub title: Option<String>,
    pub started_at: u64,
    pub last_event_at: u64,
    pub last_file_at: u64,
    pub edits: u32,
    pub reads: u32,
    /// Files this agent edited during its current turn and may come back to.
    pub holds: Vec<Hold>,
    /// The git checkout it's working in (comparable form) and that folder, as shown.
    #[serde(skip)]
    pub tree: Option<String>,
    pub tree_path: Option<String>,
    /// Branch checked out where it's working ("detached HEAD" if none).
    pub branch: Option<String>,
    /// Set when it's working in a linked worktree: that folder's name.
    pub worktree: Option<String>,
    /// The latest branch change under it, if any.
    pub branch_switch: Option<BranchSwitch>,
    /// "Wind down" (usage limit): "asked" (message waiting for its next step), "told" (it got
    /// the message), "stopped" (its turn ended after that). Cleared when you prompt it again.
    pub wind_down: Option<String>,
    /// Between the start of a turn (prompt / subagent start) and its end (Stop / SubagentStop).
    #[serde(skip)]
    pub in_turn: bool,
}

/// The parts of a hook payload we use. Built from JSON in server.rs.
#[derive(Debug, Clone, Default)]
pub struct HookEvent {
    pub event: String,
    pub session_id: String,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub cwd: Option<String>,
    pub tool_name: Option<String>,
    /// file_path / notebook_path / path from tool_input.
    pub path: Option<String>,
    pub prompt: Option<String>,
    pub title: Option<String>,
    /// Filled in by git::annotate: which checkout and branch the event came from.
    pub tree: Option<String>,
    pub tree_path: Option<String>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    /// Planetarium's own tools (mcp__planetarium__*): the collision number they refer to,
    /// and for `release`, which files (or all).
    pub collision: Option<u64>,
    pub release_files: Vec<String>,
    pub release_all: bool,
    /// The working folder as Claude Code sent it (before git::annotate moved it).
    pub cwd_original: Option<String>,
    /// SubagentStart, or the tool call that starts a subagent: its description, instructions
    /// and type.
    pub sub_description: Option<String>,
    pub sub_prompt: Option<String>,
    pub sub_type: Option<String>,
    /// The subagent was started in the background (its tool call returns right away).
    pub sub_background: bool,
}

impl HookEvent {
    pub fn agent_key(&self) -> String {
        match &self.agent_id {
            Some(id) if !id.is_empty() => format!("{}:{}", self.session_id, id),
            _ => self.session_id.clone(),
        }
    }
}

/// One message in a "Let them work it out" exchange.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TalkMessage {
    /// Agent key, or "user" for something you said from Planetarium.
    pub from: String,
    pub text: String,
    /// Replies: "go_ahead" | "wait" | "done".
    pub decision: Option<String>,
    pub at: u64,
}

/// One agent about to touch a file other agents are holding.
#[derive(Serialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Collision {
    pub id: u64,
    pub repo_id: String,
    pub file: String,
    /// The agent that's about to touch the file.
    pub agent_key: String,
    /// The agents holding it.
    pub holder_keys: Vec<String>,
    /// "here": a holder is working in this file right now; "held": changed earlier this turn.
    pub severity: String,
    /// "edit" or "read".
    pub access: String,
    /// "warn" | "ask" | "talk"
    pub mode: String,
    /// warn/ask: "warned" | "waiting" (on you) | "proceeded" | "stopped" | "expired"
    /// talk: "talking" | "asked_to_wait" | "needs_you" | "agreed" | "no_reply" | "no_tools" | "closed"
    pub status: String,
    pub created_at: u64,
    pub resolved_at: Option<u64>,
    /// What you told the agent, if anything.
    pub resolution: Option<String>,
    /// Ask mode: when Planetarium gives up waiting and falls back to a warning.
    pub deadline: Option<u64>,
    /// Talk mode: the agents' exchange, messages sent by the paused agent, and how many times
    /// it tried the edit.
    pub messages: Vec<TalkMessage>,
    pub rounds: u32,
    #[serde(skip)]
    pub attempts: u32,
    /// The checkout the paused agent is in (holds are per checkout).
    #[serde(skip)]
    pub tree: Option<String>,
}

#[derive(Default)]
pub struct AgentHub {
    agents: HashMap<String, Agent>,
    collisions: Vec<Collision>,
    next_collision: u64,
    /// Messages waiting to be delivered to an agent on its next tool use.
    notes: HashMap<String, Vec<String>>,
    /// (holder, file, editor) already told about, so a holder hears about each editor once.
    notified: HashSet<(String, String, String)>,
    /// (agent, file) edits you approved in Ask mode; not asked again until that agent's turn ends.
    approved: HashSet<(String, String)>,
    /// (tool, collision) → the agent that is calling that Planetarium tool right now. Its
    /// PreToolUse hook (which carries the session) arrives just before the tool call itself.
    mcp_callers: HashMap<(String, u64), String>,
    /// Per session: subagents asked for (type, description, instructions) that haven't
    /// started yet, matched to the next SubagentStart of the same type.
    pending_subagents: HashMap<String, VecDeque<(Option<String>, Option<String>, Option<String>)>>,
    /// One-time "wind down" message per agent, and when it stops being worth delivering.
    wind_notes: HashMap<String, (String, u64)>,
}

/// A wind-down message not delivered within this long is dropped (the moment has passed).
pub const WIND_DOWN_TTL_MS: u64 = 15 * 60_000;

/// The tool a main agent uses to start a subagent (named either way across versions).
pub fn is_subagent_tool(tool: &str) -> bool {
    tool == "Task" || tool == "Agent"
}

/// Instructions are kept to this many characters.
const INSTRUCTIONS_MAX: usize = 1200;

/// A short description from instructions, when no description was given: the first words.
fn description_from(instructions: &str) -> String {
    shorten(instructions.split(['.', '\n']).next().unwrap_or(instructions), 60)
}

fn norm(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if cfg!(windows) { s.to_lowercase() } else { s }
}

fn same_file(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) && (cfg!(windows) || a == b)
}

/// Which repo contains `path`, and the path relative to it (forward slashes, original case).
/// The longest matching repo wins, so nested repos resolve to the innermost one.
pub fn locate(repos: &[RepoMeta], path: &str) -> Option<(String, String)> {
    let target = norm(path);
    let original = path.replace('\\', "/");
    let mut best: Option<(usize, &RepoMeta)> = None;
    for r in repos {
        let root = norm(&r.path);
        let inside = target == root || (target.starts_with(&root) && target.as_bytes().get(root.len()) == Some(&b'/'));
        if inside && best.map_or(true, |(len, _)| root.len() > len) {
            best = Some((root.len(), r));
        }
    }
    let (len, repo) = best?;
    let rel = if original.len() > len { original[len..].trim_start_matches('/').to_string() } else { String::new() };
    Some((repo.id.clone(), rel))
}

/// Context an editor or Claude Code attaches to a prompt ("The user selected the lines 18 to 18
/// from …", open-file notes, reminders): dropped whole, it isn't the task.
fn is_context_tag(name: &str) -> bool {
    name.starts_with("ide_")
        || name.starts_with("ide-")
        || name == "system-reminder"
        // Notices Claude Code sends as prompts, e.g. <task-notification> when a background job ends.
        || name.contains("notification")
        || name.starts_with("task-")
        || name.starts_with("task_")
}

/// Drop markup the prompt box adds (pasted-content blocks, images, editor context, other <tags>)
/// so task text reads as plain words.
pub fn clean_task(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let tag_like = after.chars().next().map_or(false, |c| c.is_ascii_alphabetic() || c == '/');
        match (tag_like, after.find('>')) {
            (true, Some(end)) => {
                let name: String = after.trim_start_matches('/').chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
                rest = &after[end + 1..];
                // Pasted content is often long: skip its body, and leave a short marker.
                if name == "pasted_content" && !after.starts_with('/') {
                    if !after[..end].ends_with('/') {
                        if let Some(close) = rest.find("</pasted_content>") {
                            rest = &rest[close + "</pasted_content>".len()..];
                        }
                    }
                    out.push_str(" [pasted text] ");
                } else if is_context_tag(&name) && !after.starts_with('/') {
                    if !after[..end].ends_with('/') {
                        let close = format!("</{name}>");
                        rest = match rest.find(&close) {
                            Some(i) => &rest[i + close.len()..],
                            None => "",
                        };
                    }
                    out.push(' ');
                } else {
                    out.push(' ');
                }
            }
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn shorten(s: &str, max: usize) -> String {
    let s = clean_task(s);
    let one_line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        let cut: String = one_line.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

pub fn action_for(tool: &str) -> Option<&'static str> {
    match tool {
        "Read" => Some("read"),
        "Edit" | "MultiEdit" | "NotebookEdit" => Some("edit"),
        "Write" => Some("write"),
        "Glob" | "Grep" => Some("search"),
        _ => None,
    }
}

/// How an agent is named in messages and the UI.
pub fn display_name(a: &Agent) -> String {
    if a.parent_key.is_some() {
        return format!("{} subagent", a.agent_type);
    }
    match &a.title {
        Some(t) if !t.is_empty() => t.clone(),
        _ => format!("Claude · {}", a.session_id.chars().take(4).collect::<String>()),
    }
}

impl AgentHub {
    pub fn list(&self) -> Vec<Agent> {
        let mut v: Vec<Agent> = self.agents.values().cloned().collect();
        v.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.key.cmp(&b.key)));
        v
    }

    pub fn get(&self, key: &str) -> Option<&Agent> {
        self.agents.get(key)
    }

    pub fn collisions(&self) -> Vec<Collision> {
        self.collisions.clone()
    }

    fn ensure(&mut self, key: &str, ev: &HookEvent, now: u64) -> &mut Agent {
        let is_sub = key != ev.session_id;
        self.agents.entry(key.to_string()).or_insert_with(|| Agent {
            key: key.to_string(),
            session_id: ev.session_id.clone(),
            agent_id: if is_sub { ev.agent_id.clone() } else { None },
            agent_type: if is_sub { ev.agent_type.clone().unwrap_or_else(|| "subagent".into()) } else { "main".into() },
            parent_key: if is_sub { Some(ev.session_id.clone()) } else { None },
            repo_id: None,
            file: None,
            action: None,
            status: "idle".into(),
            task: None,
            description: None,
            instructions: None,
            title: None,
            started_at: now,
            last_event_at: now,
            last_file_at: 0,
            edits: 0,
            reads: 0,
            holds: Vec::new(),
            tree: None,
            tree_path: None,
            branch: None,
            worktree: None,
            branch_switch: None,
            wind_down: None,
            in_turn: false,
        })
    }

    fn clear_holds(&mut self, key: &str) {
        if let Some(a) = self.agents.get_mut(key) {
            a.holds.clear();
        }
        self.notified.retain(|(holder, _, _)| holder != key);
        self.approved.retain(|(agent, _)| agent != key);
    }

    pub fn approve(&mut self, key: &str, file: &str) {
        self.approved.insert((key.to_string(), file.to_lowercase()));
    }

    pub fn is_approved(&self, key: &str, file: &str) -> bool {
        self.approved.contains(&(key.to_string(), file.to_lowercase()))
    }

    /// Apply one hook event. Returns true if anything visible changed.
    pub fn apply(&mut self, ev: &HookEvent, repos: &[RepoMeta], now: u64) -> bool {
        if ev.session_id.is_empty() {
            return false;
        }
        let before = self.list();
        let main_key = ev.session_id.clone();
        let key = ev.agent_key();

        // Make sure the parent exists whenever a subagent shows up.
        if key != main_key {
            let parent_ev = HookEvent { agent_id: None, ..ev.clone() };
            self.ensure(&main_key, &parent_ev, now);
        }

        let cwd_repo = ev.cwd.as_deref().and_then(|c| locate(repos, c)).map(|(id, _)| id);

        // The tool call that ran a subagent returned: that subagent is finished. A backstop in
        // case its SubagentStop never arrives (background subagents return at once; skipped).
        if ev.event == "PostToolUse" && !ev.sub_background && ev.tool_name.as_deref().map_or(false, is_subagent_tool) {
            if let Some(d) = ev.sub_description.as_deref().map(|d| shorten(d, 60)) {
                let finished: Vec<String> = self
                    .agents
                    .values()
                    .filter(|a| a.session_id == ev.session_id && a.parent_key.is_some() && a.status != "done")
                    .filter(|a| a.description.as_deref() == Some(d.as_str()))
                    .map(|a| a.key.clone())
                    .collect();
                // Only when it's unambiguous.
                if let [k] = finished.as_slice() {
                    if let Some(a) = self.agents.get_mut(k) {
                        a.status = "done".into();
                        a.last_event_at = now;
                    }
                    let k = k.clone();
                    self.clear_holds(&k);
                }
            }
        }

        // A subagent is about to be started: remember what it's for until it shows up.
        if ev.event == "PreToolUse" && ev.tool_name.as_deref().map_or(false, is_subagent_tool) {
            let q = self.pending_subagents.entry(ev.session_id.clone()).or_default();
            q.push_back((ev.sub_type.clone(), ev.sub_description.clone(), ev.sub_prompt.clone()));
            while q.len() > 16 {
                q.pop_front();
            }
        }

        match ev.event.as_str() {
            "SessionStart" => {
                let a = self.ensure(&key, ev, now);
                a.last_event_at = now;
                if a.status == "done" { a.status = "idle".into(); }
                if a.repo_id.is_none() { a.repo_id = cwd_repo; }
                if ev.title.is_some() { a.title = ev.title.clone(); }
            }
            "UserPromptSubmit" => {
                let a = self.ensure(&key, ev, now);
                a.last_event_at = now;
                a.status = "working".into();
                a.in_turn = true;
                a.wind_down = None;
                // Prompts that are only system notices (e.g. a background job finished) leave the
                // task as it was.
                if let Some(t) = ev.prompt.as_deref().map(|p| shorten(p, 160)).filter(|t| !t.is_empty()) {
                    a.task = Some(t);
                }
                if a.repo_id.is_none() { a.repo_id = cwd_repo; }
            }
            "PreToolUse" | "PostToolUse" => {
                let located = ev.path.as_deref().and_then(|p| locate(repos, p));
                let action = ev.tool_name.as_deref().and_then(action_for);
                let a = self.ensure(&key, ev, now);
                a.last_event_at = now;
                a.status = "working".into();
                a.in_turn = true;
                match located {
                    Some((repo_id, rel)) => {
                        a.repo_id = Some(repo_id.clone());
                        a.file = if rel.is_empty() { None } else { Some(rel.clone()) };
                        a.last_file_at = now;
                        if let Some(act) = action {
                            a.action = Some(act.to_string());
                            // Count each tool call once (on the Post event when we get both).
                            if ev.event == "PostToolUse" {
                                match act {
                                    "edit" | "write" => {
                                        a.edits += 1;
                                        // Automatic hold: this agent may come back to this file.
                                        let tree = ev.tree.clone();
                                        if !rel.is_empty() && !a.holds.iter().any(|h| h.repo_id == repo_id && same_file(&h.file, &rel) && h.tree == tree) {
                                            a.holds.push(Hold { repo_id, file: rel, since: now, tree });
                                        }
                                    }
                                    "read" => a.reads += 1,
                                    _ => {}
                                }
                            }
                        }
                    }
                    None => {
                        if a.repo_id.is_none() { a.repo_id = cwd_repo; }
                    }
                }
            }
            "SubagentStart" => {
                // Match it to the request that started it: same type, same description if known.
                let pending = self.pending_subagents.get_mut(&ev.session_id).and_then(|q| {
                    let same_type = |t: &Option<String>| t.is_none() || ev.agent_type.is_none() || *t == ev.agent_type;
                    let i = q
                        .iter()
                        .position(|(t, d, _)| same_type(t) && ev.sub_description.is_some() && *d == ev.sub_description)
                        .or_else(|| q.iter().position(|(t, _, _)| same_type(t)))?;
                    q.remove(i)
                });
                let (_, p_desc, p_prompt) = pending.unwrap_or((None, None, None));
                let instructions = ev.sub_prompt.clone().or(p_prompt).map(|p| shorten(&p, INSTRUCTIONS_MAX)).filter(|p| !p.is_empty());
                let description = ev
                    .sub_description
                    .clone()
                    .or(p_desc)
                    .map(|d| shorten(&d, 60))
                    .filter(|d| !d.is_empty())
                    .or_else(|| instructions.as_deref().map(description_from));
                let a = self.ensure(&key, ev, now);
                a.last_event_at = now;
                a.status = "working".into();
                a.in_turn = true;
                if description.is_some() { a.description = description; }
                if instructions.is_some() { a.instructions = instructions; }
                if a.repo_id.is_none() { a.repo_id = cwd_repo; }
            }
            "SubagentStop" => {
                let a = self.ensure(&key, ev, now);
                a.last_event_at = now;
                a.status = "done".into();
                a.in_turn = false;
                self.wind_notes.remove(&key);
                let a = self.agents.get_mut(&key).unwrap();
                let parent = a.parent_key.clone();
                self.clear_holds(&key);
                // A finished subagent won't use another tool, so its waiting messages go to the
                // agent that started it.
                if let (Some(parent), Some(notes)) = (parent, self.notes.remove(&key)) {
                    for n in notes {
                        self.queue_note(&parent, n);
                    }
                }
            }
            "Stop" => {
                let a = self.ensure(&key, ev, now);
                a.last_event_at = now;
                a.status = "idle".into();
                a.file = None;
                a.in_turn = false;
                // Stopped after being asked to wind down: you'll want to tell it to continue later.
                a.wind_down = if a.wind_down.as_deref() == Some("told") { Some("stopped".into()) } else { None };
                self.wind_notes.remove(&key);
                // The turn is over: everything this agent held is released.
                self.clear_holds(&key);
            }
            "SessionEnd" => {
                let keys: Vec<String> = self.agents.values().filter(|a| a.session_id == ev.session_id).map(|a| a.key.clone()).collect();
                for k in keys {
                    if let Some(a) = self.agents.get_mut(&k) {
                        a.status = "done".into();
                        a.last_event_at = now;
                    }
                    self.clear_holds(&k);
                }
            }
            _ => return false,
        }

        // Where it's working (git checkout + branch). A branch change in that folder is applied
        // to everyone working there.
        if let (Some(tree), Some(branch)) = (ev.tree.clone(), ev.branch.clone()) {
            if ev.event != "SessionEnd" {
                self.observe_branch(&tree, &branch, now);
                if let Some(a) = self.agents.get_mut(&key) {
                    if a.tree.as_deref() != Some(tree.as_str()) {
                        // Moved to another checkout: not a switch, just a new place.
                        a.branch_switch = None;
                    }
                    a.tree = Some(tree);
                    a.tree_path = ev.tree_path.clone();
                    a.branch = Some(branch);
                    a.worktree = ev.worktree.clone();
                }
            }
        }

        // A subagent with no repo yet inherits its parent's.
        if key != main_key {
            let parent_repo = self.agents.get(&main_key).and_then(|p| p.repo_id.clone());
            if let Some(a) = self.agents.get_mut(&key) {
                if a.repo_id.is_none() { a.repo_id = parent_repo; }
            }
        }
        self.refresh_derived();
        self.list() != before
    }

    /// Other agents holding `file` in `repo_id`, from `key`'s point of view. An agent's own
    /// parent or child doesn't count (delegating work to a subagent isn't a collision), but
    /// siblings and other sessions do. Returns (holder key, is working in the file right now).
    /// Only holds made in the same checkout count (`tree`; None matches anything).
    pub fn holders_of(&self, key: &str, repo_id: &str, file: &str, tree: Option<&str>) -> Vec<(String, bool)> {
        let me = self.agents.get(key);
        // Subagent keys are "session:agent", so the parent is known even before the agent exists.
        let my_parent = me.and_then(|a| a.parent_key.clone()).or_else(|| key.split_once(':').map(|(p, _)| p.to_string()));
        let mut out: Vec<(String, bool)> = self
            .agents
            .values()
            .filter(|a| a.key != key && a.status != "done")
            .filter(|a| Some(&a.key) != my_parent.as_ref() && a.parent_key.as_deref() != Some(key))
            .filter(|a| {
                a.holds.iter().any(|h| {
                    h.repo_id == repo_id
                        && same_file(&h.file, file)
                        && (tree.is_none() || h.tree.is_none() || h.tree.as_deref() == tree)
                })
            })
            .map(|a| {
                let same_tree = tree.is_none() || a.tree.is_none() || a.tree.as_deref() == tree;
                let here = same_tree
                    && a.status == "working"
                    && a.file.as_deref().map_or(false, |f| same_file(f, file))
                    && matches!(a.action.as_deref(), Some("edit") | Some("write"));
                (a.key.clone(), here)
            })
            .collect();
        out.sort();
        out
    }

    /// The checkouts agents are working in right now (for the branch check in server.rs).
    pub fn active_trees(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .agents
            .values()
            .filter(|a| a.status != "done")
            .filter_map(|a| Some((a.tree.clone()?, a.tree_path.clone()?)))
            .collect();
        v.sort();
        v.dedup_by(|x, y| x.0 == y.0);
        v
    }

    /// The branch checked out in `tree` is `branch`. Anyone working there who saw a different
    /// branch is flagged and told (the files under them may have changed). Returns true if
    /// anything changed.
    pub fn observe_branch(&mut self, tree: &str, branch: &str, now: u64) -> bool {
        let mut told = Vec::new();
        for a in self.agents.values_mut() {
            if a.status == "done" || a.tree.as_deref() != Some(tree) {
                continue;
            }
            match a.branch.clone() {
                Some(old) if old != branch => {
                    a.branch = Some(branch.to_string());
                    a.branch_switch = Some(BranchSwitch { from: old.clone(), to: branch.to_string(), at: now });
                    told.push((a.key.clone(), old, a.tree_path.clone().unwrap_or_default()));
                }
                None => a.branch = Some(branch.to_string()),
                _ => {}
            }
        }
        let changed = !told.is_empty();
        for (key, old, folder) in told {
            self.queue_note(&key, format!(
                "[Planetarium] The branch checked out in {folder} changed from {old} to {branch} while you were working there (someone ran git checkout or git switch). Files may have changed under you: read any file again before editing it, and if you didn't expect this, stop and tell the user."
            ));
        }
        changed
    }

    /// Drop holds: the given (repo, file, checkout) ones, or all of them. Returns the files
    /// released and the ones it wasn't holding.
    pub fn release(&mut self, key: &str, items: &[(String, String, Option<String>)], all: bool) -> (Vec<String>, Vec<String>) {
        let Some(a) = self.agents.get_mut(key) else { return (Vec::new(), items.iter().map(|i| i.1.clone()).collect()) };
        if all {
            let released = a.holds.drain(..).map(|h| h.file).collect();
            return (released, Vec::new());
        }
        let (mut released, mut missing) = (Vec::new(), Vec::new());
        for (repo_id, file, tree) in items {
            let before = a.holds.len();
            a.holds.retain(|h| !(h.repo_id == *repo_id && same_file(&h.file, file) && (tree.is_none() || h.tree.is_none() || h.tree == *tree)));
            if a.holds.len() < before { released.push(file.clone()) } else { missing.push(file.clone()) }
        }
        (released, missing)
    }

    /// Ask every agent that's in the middle of a turn to wind down. The message reaches each on
    /// its next step (never at the start of a later turn). Returns how many were asked.
    pub fn wind_down(&mut self, text: &str, now: u64) -> usize {
        let keys: Vec<String> = self.agents.values().filter(|a| a.in_turn && a.status != "done").map(|a| a.key.clone()).collect();
        for k in &keys {
            self.wind_notes.insert(k.clone(), (text.to_string(), now + WIND_DOWN_TTL_MS));
            if let Some(a) = self.agents.get_mut(k) {
                a.wind_down = Some("asked".into());
            }
        }
        keys.len()
    }

    /// The wind-down message for this agent, if one is waiting (delivered once).
    pub fn take_wind_note(&mut self, key: &str, now: u64) -> Option<String> {
        let (text, expires) = self.wind_notes.remove(key)?;
        if now > expires {
            return None;
        }
        if let Some(a) = self.agents.get_mut(key) {
            a.wind_down = Some("told".into());
        }
        Some(text)
    }

    pub fn set_mcp_caller(&mut self, tool: &str, collision: u64, key: &str) {
        if self.mcp_callers.len() > 256 {
            self.mcp_callers.clear();
        }
        self.mcp_callers.insert((tool.to_string(), collision), key.to_string());
    }

    pub fn take_mcp_caller(&mut self, tool: &str, collision: u64) -> Option<String> {
        self.mcp_callers.remove(&(tool.to_string(), collision))
    }

    pub fn collision(&self, id: u64) -> Option<&Collision> {
        self.collisions.iter().find(|c| c.id == id)
    }

    pub fn add_collision(&mut self, mut c: Collision) -> u64 {
        self.next_collision += 1;
        c.id = self.next_collision;
        let id = c.id;
        self.collisions.push(c);
        id
    }

    pub fn update_collision(&mut self, id: u64, f: impl FnOnce(&mut Collision)) {
        if let Some(c) = self.collisions.iter_mut().find(|c| c.id == id) {
            f(c);
        }
    }

    /// Queue a message for an agent; it's delivered with the reply to its next tool use.
    pub fn queue_note(&mut self, key: &str, note: String) {
        let list = self.notes.entry(key.to_string()).or_default();
        if !list.contains(&note) {
            list.push(note);
        }
    }

    pub fn take_notes(&mut self, key: &str) -> Vec<String> {
        self.notes.remove(key).unwrap_or_default()
    }

    /// Returns true the first time `editor` is reported to `holder` for `file` this turn.
    pub fn first_notice(&mut self, holder: &str, file: &str, editor: &str) -> bool {
        self.notified.insert((holder.to_string(), file.to_lowercase(), editor.to_string()))
    }

    /// Time-based changes: idle after a quiet spell, "waiting" for subagents, forgetting the dead,
    /// expiring old collisions. Returns true if anything visible changed.
    pub fn tick(&mut self, now: u64) -> bool {
        let before = (self.list(), self.collisions.clone());
        let mut forgotten = Vec::new();
        self.agents.retain(|k, a| {
            let age = now.saturating_sub(a.last_event_at);
            let keep = !(a.status == "done" && age > DONE_LINGER_MS) && age < FORGET_AFTER_MS;
            if !keep { forgotten.push(k.clone()); }
            keep
        });
        for k in forgotten {
            self.notes.remove(&k);
            self.notified.retain(|(holder, _, _)| *holder != k);
            self.approved.retain(|(agent, _)| *agent != k);
        }
        let expired: Vec<String> = self.wind_notes.iter().filter(|(_, (_, exp))| now > *exp).map(|(k, _)| k.clone()).collect();
        for k in expired {
            self.wind_notes.remove(&k);
            if let Some(a) = self.agents.get_mut(&k) {
                if a.wind_down.as_deref() == Some("asked") {
                    a.wind_down = None;
                }
            }
        }
        for a in self.agents.values_mut() {
            if a.status == "working" && now.saturating_sub(a.last_event_at) > IDLE_AFTER_MS {
                a.status = "idle".into();
                a.file = None;
            }
        }
        self.collisions.retain(|c| {
            crate::talk::is_open(c) || c.status == "waiting" || now.saturating_sub(c.resolved_at.unwrap_or(c.created_at)) < COLLISION_LINGER_MS
        });
        self.refresh_derived();
        (self.list(), self.collisions.clone()) != before
    }

    fn refresh_derived(&mut self) {
        // A main agent counts as "waiting" while it's not working itself but has subagents that are.
        let busy_parents: Vec<String> = self
            .agents
            .values()
            .filter(|a| a.parent_key.is_some() && a.status == "working")
            .filter_map(|a| a.parent_key.clone())
            .collect();
        for a in self.agents.values_mut() {
            if a.parent_key.is_some() { continue; }
            let has_busy = busy_parents.contains(&a.key);
            if has_busy && a.status == "idle" {
                a.status = "waiting".into();
            } else if !has_busy && a.status == "waiting" {
                a.status = "idle".into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn subagents_get_their_description_and_instructions() {
        let r = vec![RepoMeta { id: "salsa".into(), path: r"C:\Users\me\source\repos\salsa".into(), name: "salsa".into(), color_index: 0, added_at: 1 }];
        let mut hub = AgentHub::default();
        let start = |hub: &mut AgentHub, ty: &str, desc: &str, prompt: &str, t: u64| {
            let mut e = ev("PreToolUse", "S", None, Some("Task"), None);
            e.sub_type = Some(ty.into());
            e.sub_description = Some(desc.into());
            e.sub_prompt = Some(prompt.into());
            hub.apply(&e, &r, t);
        };
        // Two launched back to back; they start in order, possibly without a description.
        start(&mut hub, "general-purpose", "Fix blend mode bug", "The blend mode picked in Ephemera Place is never applied. Find where...", 1);
        start(&mut hub, "Explore", "Map ShapeManager usage", "List every ShapeManager member Frogmarks uses.", 2);
        let mut s1 = ev("SubagentStart", "S", Some("x1"), None, None);
        s1.agent_type = Some("Explore".into());
        hub.apply(&s1, &r, 3);
        let mut s2 = ev("SubagentStart", "S", Some("x2"), None, None);
        s2.agent_type = Some("general-purpose".into());
        hub.apply(&s2, &r, 4);
        assert_eq!(hub.get("S:x1").unwrap().description.as_deref(), Some("Map ShapeManager usage"));
        assert_eq!(hub.get("S:x2").unwrap().description.as_deref(), Some("Fix blend mode bug"));
        assert!(hub.get("S:x2").unwrap().instructions.as_deref().unwrap().starts_with("The blend mode"));
        // A description sent with SubagentStart itself wins.
        let mut s3 = ev("SubagentStart", "S", Some("x3"), None, None);
        s3.agent_type = Some("Plan".into());
        s3.sub_description = Some("Plan the Skins extraction".into());
        hub.apply(&s3, &r, 5);
        assert_eq!(hub.get("S:x3").unwrap().description.as_deref(), Some("Plan the Skins extraction"));

        // x2's tool call returns: it's finished even if its SubagentStop never arrives.
        let mut edit = ev("PostToolUse", "S", Some("x2"), Some("Edit"), Some(r"C:\Users\me\source\repos\salsa\a.ts"));
        edit.agent_type = Some("general-purpose".into());
        hub.apply(&edit, &r, 6);
        assert_eq!(hub.get("S:x2").unwrap().holds.len(), 1);
        let mut done = ev("PostToolUse", "S", None, Some("Task"), None);
        done.sub_description = Some("Fix blend mode bug".into());
        hub.apply(&done, &r, 7);
        assert_eq!(hub.get("S:x2").unwrap().status, "done");
        assert!(hub.get("S:x2").unwrap().holds.is_empty());
        assert_ne!(hub.get("S:x1").unwrap().status, "done");
    }

    #[test]
    fn task_text_drops_markup() {
        assert_eq!(
            super::shorten("fix <pasted_content id=\"9a87\">lots\nof text</pasted_content> the bug", 160),
            "fix [pasted text] the bug"
        );
        assert_eq!(super::shorten("see <pasted_content id=\"1\"/> and <image>", 160), "see [pasted text] and");
        assert_eq!(super::shorten("is a < b and 3<4?", 160), "is a < b and 3<4?");
        assert_eq!(
            super::shorten("<ide_selection>The user selected the lines 18 to 18 from c:\\x.ts:\nfoo\n</ide_selection>\nwhy is this slow?", 160),
            "why is this slow?"
        );
        assert_eq!(super::shorten("<ide_opened_file>The user opened a.ts</ide_opened_file>", 160), "");
        assert_eq!(super::shorten("<task-notification><task-id>ada0e568</task-id><tool-use-id>toolu_01AR</tool-use-id></task-notification>", 160), "");
    }

    use super::*;

    fn repos() -> Vec<RepoMeta> {
        vec![
            RepoMeta { id: "salsa".into(), path: r"C:\Users\me\source\repos\salsa".into(), name: "salsa".into(), color_index: 0, added_at: 1 },
            RepoMeta { id: "fm".into(), path: r"C:\Users\me\source\repos\Frogmarks".into(), name: "Frogmarks".into(), color_index: 1, added_at: 2 },
            RepoMeta { id: "fm-inner".into(), path: r"C:\Users\me\source\repos\Frogmarks\Frogmarks".into(), name: "Frogmarks".into(), color_index: 2, added_at: 3 },
        ]
    }

    fn ev(event: &str, sid: &str, aid: Option<&str>, tool: Option<&str>, path: Option<&str>) -> HookEvent {
        HookEvent {
            event: event.into(),
            session_id: sid.into(),
            agent_id: aid.map(Into::into),
            agent_type: aid.map(|_| "Explore".into()),
            cwd: Some(r"C:\Users\me\source\repos\salsa".into()),
            tool_name: tool.map(Into::into),
            path: path.map(Into::into),
            prompt: None,
            title: None,
            ..Default::default()
        }
    }

    #[test]
    fn locates_innermost_repo() {
        let r = repos();
        assert_eq!(locate(&r, r"C:\Users\me\source\repos\salsa\src\main.ts"), Some(("salsa".into(), "src/main.ts".into())));
        assert_eq!(locate(&r, r"C:\Users\me\source\repos\Frogmarks\Frogmarks\Program.cs"), Some(("fm-inner".into(), "Program.cs".into())));
        assert_eq!(locate(&r, r"C:\Users\me\source\repos\salsa-old\x.ts"), None);
        assert_eq!(locate(&r, r"C:\Users\me\source\repos\salsa"), Some(("salsa".into(), String::new())));
    }

    #[test]
    fn holds_until_turn_ends() {
        let r = repos();
        let mut hub = AgentHub::default();
        let f = r"C:\Users\me\source\repos\salsa\src\view.ts";
        hub.apply(&ev("PostToolUse", "A", None, Some("Edit"), Some(f)), &r, 1);
        hub.apply(&ev("PostToolUse", "A", None, Some("Read"), Some(r"C:\Users\me\source\repos\salsa\src\main.ts")), &r, 2);
        // A moved on, but still holds view.ts.
        assert_eq!(hub.holders_of("B", "salsa", "src/view.ts", None), vec![("A".to_string(), false)]);
        hub.apply(&ev("PreToolUse", "A", None, Some("Edit"), Some(f)), &r, 3);
        assert_eq!(hub.holders_of("B", "salsa", "src/view.ts", None), vec![("A".to_string(), true)]);
        hub.apply(&ev("Stop", "A", None, None, None), &r, 4);
        assert!(hub.holders_of("B", "salsa", "src/view.ts", None).is_empty());
    }

    #[test]
    fn parent_and_child_are_not_collisions_but_siblings_are() {
        let r = repos();
        let mut hub = AgentHub::default();
        let f = r"C:\Users\me\source\repos\salsa\src\view.ts";
        hub.apply(&ev("PostToolUse", "S", None, Some("Edit"), Some(f)), &r, 1);
        hub.apply(&ev("PostToolUse", "S", Some("x1"), Some("Edit"), Some(f)), &r, 2);
        assert!(hub.holders_of("S:x1", "salsa", "src/view.ts", None).is_empty(), "child vs parent");
        assert!(hub.holders_of("S", "salsa", "src/view.ts", None).is_empty(), "parent vs child");
        assert_eq!(hub.holders_of("S:x2", "salsa", "src/view.ts", None), vec![("S:x1".to_string(), true)], "sibling (x1 is still at the file)");
    }
}
