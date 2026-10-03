//! server.rs — the local endpoint Claude Code's hooks post to.
//!
//! Listens on 127.0.0.1 only (nothing outside this computer can reach it). Almost every request
//! is answered immediately: an empty 200 ("no opinion"), or a note for the agent (coord.rs).
//! The one exception is an Ask-mode collision, where the reply waits for your decision in
//! Planetarium, on its own thread, so other agents are never held up by it.
//! If Planetarium isn't running, Claude Code gets "connection refused", which it ignores.
//!
//! The same port serves Planetarium's tools for agents at /mcp (mcp.rs). Requests that carry a
//! browser Origin header are refused, so a web page can't pose as an agent.

use crate::agents::{locate, HookEvent};
use crate::coord::{self, Decision, Reply};
use crate::mcp;
use crate::talk;
use crate::git::{self, GitCache};
use crate::scanner::now_ms;
use crate::AppState;
use serde_json::Value;
use std::io::Read;
use std::collections::HashMap;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub const PORT: u16 = 47615;
pub const HOOK_PATH: &str = "/planetarium/hook";
/// Planetarium's status line posts Claude Code's status JSON here (usage limits).
pub const STATUSLINE_PATH: &str = "/planetarium/statusline";

pub fn statusline_url() -> String {
    format!("http://127.0.0.1:{PORT}{STATUSLINE_PATH}")
}

/// The latest plan usage any session reported.
fn usage_store() -> &'static Mutex<Option<crate::usage::Usage>> {
    static U: OnceLock<Mutex<Option<crate::usage::Usage>>> = OnceLock::new();
    U.get_or_init(|| Mutex::new(None))
}

pub fn current_usage() -> Option<crate::usage::Usage> {
    usage_store().lock().unwrap().clone()
}

pub fn hook_url() -> String {
    format!("http://127.0.0.1:{PORT}{HOOK_PATH}")
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(|x| x.as_str()).map(|s| s.to_string()).filter(|s| !s.is_empty())
}

/// Pull the fields we care about out of a hook payload. Everything else (including the text of
/// edits) is dropped right here and never stored.
pub fn parse_hook(body: &str) -> Option<HookEvent> {
    let v: Value = serde_json::from_str(body).ok()?;
    let input = v.get("tool_input");
    let path = input.and_then(|i| {
        str_field(i, "file_path").or_else(|| str_field(i, "notebook_path")).or_else(|| str_field(i, "path"))
    });
    let tool = str_field(&v, "tool_name").unwrap_or_default();
    let ours = tool.starts_with(coord::TOOL_PREFIX);
    let is_sub_tool = crate::agents::is_subagent_tool(&tool);
    let (collision, release_files, release_all) = match (ours, input) {
        (true, Some(i)) => (
            mcp::collision_arg(i),
            i.get("files")
                .and_then(|f| f.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .or_else(|| i.get("files").and_then(|f| f.as_str()).map(|s| vec![s.to_string()]))
                .unwrap_or_default(),
            i.get("all").and_then(|a| a.as_bool()).unwrap_or(false),
        ),
        _ => (None, Vec::new(), false),
    };
    Some(HookEvent {
        event: str_field(&v, "hook_event_name")?,
        session_id: str_field(&v, "session_id")?,
        agent_id: str_field(&v, "agent_id"),
        agent_type: str_field(&v, "agent_type"),
        cwd: str_field(&v, "cwd"),
        tool_name: str_field(&v, "tool_name"),
        path,
        prompt: str_field(&v, "user_prompt").or_else(|| str_field(&v, "prompt")),
        title: str_field(&v, "session_title"),
        collision,
        release_files,
        release_all,
        sub_description: if is_sub_tool { input.and_then(|i| str_field(i, "description")) } else { str_field(&v, "description") },
        sub_prompt: if is_sub_tool { input.and_then(|i| str_field(i, "prompt")) } else { str_field(&v, "prompt").filter(|_| str_field(&v, "hook_event_name").as_deref() == Some("SubagentStart")) },
        sub_type: if is_sub_tool { input.and_then(|i| str_field(i, "subagent_type")) } else { None },
        sub_background: is_sub_tool && input.and_then(|i| i.get("run_in_background")).and_then(|b| b.as_bool()).unwrap_or(false),
        ..Default::default()
    })
}

pub fn publish(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (agents, collisions) = {
        let hub = state.agents.lock().unwrap();
        (hub.list(), hub.collisions())
    };
    let _ = app.emit("agents:updated", agents);
    let _ = app.emit("collisions:updated", collisions);
}

fn respond(request: tiny_http::Request, body: Option<Value>) {
    let response = match body {
        Some(v) => {
            let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap();
            tiny_http::Response::from_string(v.to_string()).with_header(header).boxed()
        }
        None => tiny_http::Response::empty(tiny_http::StatusCode(200)).boxed(),
    };
    let _ = request.respond(response);
}

fn git_cache() -> &'static Mutex<GitCache> {
    static CACHE: OnceLock<Mutex<GitCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(GitCache::default()))
}

/// Agents waiting inside message_agent for an answer: collision → where to send it.
fn talk_waiters() -> &'static Mutex<HashMap<u64, Sender<String>>> {
    static W: OnceLock<Mutex<HashMap<u64, Sender<String>>>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Get a talk answer to the paused agent: straight into its waiting message_agent call if
/// there is one, otherwise with its next step.
pub fn deliver_talk(app: &AppHandle, collision: u64, agent_key: &str, text: String) {
    let waiter = talk_waiters().lock().unwrap().remove(&collision);
    let undelivered = match waiter {
        Some(tx) => tx.send(text).err().map(|e| e.0),
        None => Some(text),
    };
    if let Some(text) = undelivered {
        app.state::<AppState>().agents.lock().unwrap().queue_note(agent_key, text);
    }
}

/// Settle open talk exchanges whose file is no longer held, and tell the paused agents.
/// Returns true if any were settled.
fn recheck_talks(app: &AppHandle) -> bool {
    let settled = {
        let state = app.state::<AppState>();
        let mut hub = state.agents.lock().unwrap();
        talk::recheck(&mut hub, now_ms())
    };
    let any = !settled.is_empty();
    for (id, key, text) in settled {
        deliver_talk(app, id, &key, text);
    }
    any
}

/// The files a `release` call names, as (repo, repo-relative path, checkout).
fn resolve_release(ev: &HookEvent, repos: &[crate::store::RepoMeta], now: u64) -> Vec<(String, String, Option<String>)> {
    let mut cache = git_cache().lock().unwrap();
    ev.release_files
        .iter()
        .filter_map(|f| {
            let mut p = std::path::PathBuf::from(f);
            if p.is_relative() {
                p = std::path::PathBuf::from(ev.cwd_original.as_deref().or(ev.cwd.as_deref())?).join(p);
            }
            let p = p.to_string_lossy().replace('\\', "/");
            let co = cache.checkout_for(&p, now);
            let moved = co.as_ref().and_then(|c| Some(git::rebase(&p, &c.root, c.main_root.as_ref()?)?)).unwrap_or(p);
            let (repo, rel) = locate(repos, &moved)?;
            Some((repo, rel, co.map(|c| git::tree_key(&c.root))))
        })
        .collect()
}

/// Process one hook payload: update the agent picture and decide the reply.
fn process(app: &AppHandle, ev: &HookEvent) -> Reply {
    // Which checkout/branch it's in, and worktree paths moved onto the repo you added. This reads
    // a few small git files, so it's done before taking any of the shared locks.
    let mut ev = ev.clone();
    ev.cwd_original = ev.cwd.clone();
    git::annotate(&mut ev, &mut git_cache().lock().unwrap(), now_ms());
    let ev = &ev;
    let state = app.state::<AppState>();
    let repos = state.store.lock().unwrap().list();
    let settings = state.settings.lock().unwrap().value.clone();
    let repo_file = ev.path.as_deref().and_then(|p| locate(&repos, p));
    let release = if ev.release_files.is_empty() { Vec::new() } else { resolve_release(ev, &repos, now_ms()) };
    let mode = settings.mode_for(repo_file.as_ref().map(|(id, _)| id.as_str()));
    let mut hub = state.agents.lock().unwrap();
    let now = now_ms();
    hub.apply(ev, &repos, now);
    coord::handle_with_release(&mut hub, ev, repo_file, &release, &mode, settings.ask_timeout_secs * 1000, now)
}

fn respond_status(request: tiny_http::Request, code: u16) {
    let _ = request.respond(tiny_http::Response::empty(tiny_http::StatusCode(code)));
}

/// One request to /mcp.
fn serve_mcp(app: &AppHandle, request: tiny_http::Request, body: &str) {
    let (id, name, args) = match mcp::handle(body) {
        mcp::Action::Respond(v) => return respond(request, Some(v)),
        mcp::Action::Accepted => return respond_status(request, 202),
        mcp::Action::Call { id, name, args } => (id, name, args),
    };
    let collision = mcp::collision_arg(&args);
    let now = now_ms();
    match name.as_str() {
        // The work happens in this call's PostToolUse hook, which knows which agent it is.
        "release" => respond(request, Some(mcp::tool_result(&id, "OK. Planetarium will confirm which files were released.", false))),
        "message_agent" => {
            let Some(cid) = collision else {
                return respond(request, Some(mcp::tool_result(&id, "Pass the collision number from Planetarium's message.", true)));
            };
            let result = {
                let state = app.state::<AppState>();
                let mut hub = state.agents.lock().unwrap();
                let caller = hub.take_mcp_caller("message_agent", cid);
                talk::message(&mut hub, cid, caller.as_deref(), mcp::str_arg(&args, "message"), now)
            };
            publish(app);
            match result {
                Err(text) => respond(request, Some(mcp::tool_result(&id, &text, true))),
                Ok(talk::Sent::Now(text)) => respond(request, Some(mcp::tool_result(&id, &text, false))),
                Ok(talk::Sent::Wait) => {
                    // Wait for the answer on its own thread so other agents aren't held up.
                    let (tx, rx) = channel::<String>();
                    talk_waiters().lock().unwrap().insert(cid, tx);
                    let app2 = app.clone();
                    std::thread::spawn(move || {
                        let text = match rx.recv_timeout(Duration::from_millis(talk::REPLY_WAIT_MS)) {
                            Ok(t) => t,
                            Err(_) => {
                                talk_waiters().lock().unwrap().remove(&cid);
                                let state = app2.state::<AppState>();
                                let mut hub = state.agents.lock().unwrap();
                                talk::timeout(&mut hub, cid, now_ms())
                                    .unwrap_or_else(|| "[Planetarium] The exchange moved on while you waited; try your edit again to see where it stands.".into())
                            }
                        };
                        respond(request, Some(mcp::tool_result(&id, &text, false)));
                        publish(&app2);
                    });
                }
            }
        }
        _ => {
            // reply_to_agent
            let Some(cid) = collision else {
                return respond(request, Some(mcp::tool_result(&id, "Pass the collision number from Planetarium's message.", true)));
            };
            let result = {
                let state = app.state::<AppState>();
                let mut hub = state.agents.lock().unwrap();
                let caller = hub.take_mcp_caller("reply_to_agent", cid);
                let requester = hub.collision(cid).map(|c| c.agent_key.clone());
                talk::reply(&mut hub, cid, caller.as_deref(), mcp::str_arg(&args, "decision"), mcp::str_arg(&args, "message"), now)
                    .map(|(to_requester, ack)| (requester, to_requester, ack))
            };
            match result {
                Err(text) => respond(request, Some(mcp::tool_result(&id, &text, true))),
                Ok((requester, to_requester, ack)) => {
                    if let Some(r) = requester {
                        deliver_talk(app, cid, &r, to_requester);
                    }
                    respond(request, Some(mcp::tool_result(&id, &ack, false)));
                }
            }
            publish(app);
        }
    }
}

/// Start the hook server and the once-a-second housekeeping tick. Returns an error message if
/// the port is taken (the app keeps working; agents just won't show up).
pub fn start(app: AppHandle) -> Result<(), String> {
    let server = tiny_http::Server::http(("127.0.0.1", PORT)).map_err(|e| format!("Couldn't listen on port {PORT}: {e}"))?;

    let http_app = app.clone();
    std::thread::spawn(move || {
        for mut request in server.incoming_requests() {
            // Browsers always send Origin on cross-site requests; Claude Code doesn't. Refusing
            // them keeps web pages from posing as agents.
            if request.headers().iter().any(|h| h.field.equiv("Origin")) {
                respond_status(request, 403);
                continue;
            }
            let is_post = *request.method() == tiny_http::Method::Post;
            let url = request.url().split('?').next().unwrap_or("").to_string();
            if url == mcp::PATH {
                if !is_post {
                    // No server-sent events stream: plain JSON responses only.
                    respond_status(request, 405);
                    continue;
                }
                let mut body = String::new();
                let _ = request.as_reader().take(1024 * 1024).read_to_string(&mut body);
                serve_mcp(&http_app, request, &body);
                continue;
            }
            if url == STATUSLINE_PATH && is_post {
                let mut body = String::new();
                let _ = request.as_reader().take(256 * 1024).read_to_string(&mut body);
                let now = now_ms();
                let usage = crate::usage::parse(&body, now);
                if let Some(u) = &usage {
                    *usage_store().lock().unwrap() = Some(u.clone());
                    let _ = http_app.emit("usage:updated", u.clone());
                }
                // What Claude Code shows in its status line.
                let text = crate::usage::status_text(usage.as_ref().or(current_usage().as_ref()), now);
                let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/plain; charset=utf-8"[..]).unwrap();
                let _ = request.respond(tiny_http::Response::from_string(text).with_header(header));
                continue;
            }
            let is_hook = is_post && url.starts_with(HOOK_PATH);
            if !is_hook {
                let status: u16 = if request.url() == "/planetarium/ping" { 200 } else { 404 };
                let _ = request.respond(tiny_http::Response::empty(tiny_http::StatusCode(status)));
                continue;
            }
            let mut body = String::new();
            let _ = request.as_reader().take(4 * 1024 * 1024).read_to_string(&mut body);
            let Some(ev) = parse_hook(&body) else {
                respond(request, None);
                continue;
            };

            let ends_something = matches!(ev.event.as_str(), "Stop" | "SubagentStop" | "SessionEnd")
                || (ev.event == "PostToolUse" && ev.tool_name.as_deref() == Some("mcp__planetarium__release"));
            match process(&http_app, &ev) {
                Reply::Now(reply) => {
                    respond(request, reply);
                    if ends_something {
                        // A file may have been freed up for an agent waiting on it.
                        recheck_talks(&http_app);
                    }
                    publish(&http_app);
                }
                Reply::Ask { collision_id, fallback, deadline_ms } => {
                    // Park this one request until you decide (or the deadline passes).
                    let (tx, rx) = channel::<Decision>();
                    http_app.state::<AppState>().pending.lock().unwrap().insert(collision_id, tx);
                    publish(&http_app);
                    let _ = http_app.emit("collision:ask", collision_id);
                    // Bring Planetarium forward: an agent is waiting on you.
                    crate::show_main_window(&http_app);
                    let app2 = http_app.clone();
                    std::thread::spawn(move || {
                        let decision = match rx.recv_timeout(Duration::from_millis(deadline_ms)) {
                            Ok(d) => Some(d),
                            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => None,
                        };
                        let state = app2.state::<AppState>();
                        state.pending.lock().unwrap().remove(&collision_id);
                        let reply = {
                            let mut hub = state.agents.lock().unwrap();
                            match &decision {
                                Some(d) => coord::resolve(&mut hub, collision_id, d, fallback.clone(), now_ms()),
                                None => {
                                    coord::expire(&mut hub, collision_id, now_ms());
                                    fallback
                                }
                            }
                        };
                        respond(request, reply);
                        publish(&app2);
                    });
                }
            }
        }
    });

    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let state = app.state::<AppState>();
        // Has the branch changed in any folder agents are working in? (Read outside the lock.)
        let trees = state.agents.lock().unwrap().active_trees();
        let branches: Vec<(String, String)> = trees
            .into_iter()
            .filter_map(|(tree, path)| Some((tree, git::find(std::path::Path::new(&path))?.branch)))
            .collect();
        let changed = {
            let mut hub = state.agents.lock().unwrap();
            let mut changed = false;
            for (tree, branch) in &branches {
                changed |= hub.observe_branch(tree, branch, now_ms());
            }
            hub.tick(now_ms()) || changed
        };
        let settled = recheck_talks(&app);
        if changed || settled {
            publish(&app);
        }
    });
    Ok(())
}
