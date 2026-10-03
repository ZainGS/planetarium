//! Planetarium (Tauri edition): window, tray, folder access, repo scanning and the agent
//! daemon, all in one process.
//!
//! Closing the window hides it to the system tray instead of quitting, so the hook server
//! (server.rs) keeps receiving agent events in the background. Quit from the tray menu.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agents;
mod claude_settings;
mod coord;
mod git;
mod mcp;
mod scanner;
mod server;
mod settings;
mod store;
mod talk;
mod usage;
mod watch;

use notify::RecommendedWatcher;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use store::{AddError, RepoMeta, RepoStore};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;

pub struct AppState {
    pub store: Mutex<RepoStore>,
    pub agents: Mutex<agents::AgentHub>,
    pub settings: Mutex<settings::SettingsStore>,
    /// Ask-mode collisions waiting for your decision: collision id → the paused request's channel.
    pub pending: Mutex<HashMap<u64, Sender<coord::Decision>>>,
    /// Set if the hook server couldn't start (e.g. the port is taken).
    server_error: Mutex<Option<String>>,
    watchers: Mutex<HashMap<String, RecommendedWatcher>>,
    rescan_tx: Mutex<Sender<String>>,
}

impl AppState {
    fn watch(&self, repo: &RepoMeta) {
        let tx = self.rescan_tx.lock().unwrap().clone();
        if let Some(w) = watch::start_watcher(repo, tx) {
            self.watchers.lock().unwrap().insert(repo.id.clone(), w);
        }
    }

    fn unwatch(&self, id: &str) {
        self.watchers.lock().unwrap().remove(id); // dropping the watcher stops it
    }
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct AddResult {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<RepoMeta>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    canceled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_id: Option<String>,
}

// ── Commands (called from web/bridge.js) ─────────────────────────────────────────

#[tauri::command]
fn list_repos(state: State<'_, AppState>) -> Vec<RepoMeta> {
    state.store.lock().unwrap().list()
}

#[tauri::command]
async fn add_repo(app: AppHandle, state: State<'_, AppState>, folder: Option<String>) -> Result<AddResult, String> {
    let target = match folder.filter(|f| !f.trim().is_empty()) {
        Some(f) => f,
        None => {
            // Async commands run off the main thread, so the blocking picker is fine here.
            let picked = app
                .dialog()
                .file()
                .set_title("Add a repository to Planetarium")
                .blocking_pick_folder();
            match picked.and_then(|p| p.into_path().ok()) {
                Some(p) => p.to_string_lossy().into_owned(),
                None => return Ok(AddResult { canceled: true, ..Default::default() }),
            }
        }
    };

    let added = state.store.lock().unwrap().add(&target);
    match added {
        Ok(repo) => {
            state.watch(&repo);
            Ok(AddResult { ok: true, repo: Some(repo), ..Default::default() })
        }
        Err(e) => {
            let (code, repo_id) = match &e {
                AddError::Duplicate { id, .. } => (Some("DUPLICATE".to_string()), Some(id.clone())),
                _ => (None, None),
            };
            Ok(AddResult { error: Some(e.message()), code, repo_id, ..Default::default() })
        }
    }
}

#[tauri::command]
fn remove_repo(state: State<'_, AppState>, id: String) -> bool {
    state.unwatch(&id);
    state.store.lock().unwrap().remove(&id)
}

#[tauri::command]
async fn scan_repo(state: State<'_, AppState>, id: String) -> Result<scanner::ScanResult, String> {
    let repo = state.store.lock().unwrap().get(&id);
    let Some(repo) = repo else {
        return Ok(scanner::ScanResult {
            ok: false,
            repo_id: id,
            files: vec![],
            truncated: false,
            method: "walk".into(),
            git_roots: vec![],
            scanned_at: scanner::now_ms(),
            error: Some("Unknown repo".into()),
        });
    };
    // Scanning can take a moment on big repos; keep it off the async runtime's worker threads.
    tauri::async_runtime::spawn_blocking(move || scanner::scan_repo(&repo.id, &PathBuf::from(&repo.path)))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn list_agents(state: State<'_, AppState>) -> Vec<agents::Agent> {
    state.agents.lock().unwrap().list()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeInfo {
    #[serde(flatten)]
    status: claude_settings::ClaudeStatus,
    /// Why the hook server isn't running, if it isn't.
    server_error: Option<String>,
}

fn claude_info(state: &AppState, status: claude_settings::ClaudeStatus) -> ClaudeInfo {
    ClaudeInfo { status, server_error: state.server_error.lock().unwrap().clone() }
}

#[tauri::command]
fn claude_status(state: State<'_, AppState>) -> ClaudeInfo {
    claude_info(&state, claude_settings::status())
}

#[tauri::command]
fn claude_connect(state: State<'_, AppState>) -> Result<ClaudeInfo, String> {
    claude_settings::connect().map(|s| claude_info(&state, s))
}

#[tauri::command]
fn claude_disconnect(state: State<'_, AppState>) -> Result<ClaudeInfo, String> {
    claude_settings::disconnect().map(|s| claude_info(&state, s))
}

#[tauri::command]
fn list_collisions(state: State<'_, AppState>) -> Vec<agents::Collision> {
    state.agents.lock().unwrap().collisions()
}

/// Your answer to an Ask-mode collision. Returns false if it was already answered or timed out.
#[tauri::command]
fn resolve_collision(app: AppHandle, state: State<'_, AppState>, id: u64, action: String, message: Option<String>, note_for_holders: Option<String>) -> bool {
    // An Ask-mode edit waiting on you.
    let tx = state.pending.lock().unwrap().remove(&id);
    if let Some(tx) = tx {
        return tx.send(coord::Decision { action, message, note_for_holders }).is_ok();
    }
    // A "Let them work it out" exchange you're stepping into.
    let (decided, requester) = {
        let mut hub = state.agents.lock().unwrap();
        let requester = hub.collision(id).map(|c| (c.agent_key.clone(), c.holder_keys.clone(), c.file.clone()));
        let text = talk::user_decision(&mut hub, id, &action, message.as_deref(), scanner::now_ms());
        if let (Some(_), Some((_, holders, file)), Some(note)) = (&text, &requester, note_for_holders.as_deref().map(str::trim).filter(|n| !n.is_empty())) {
            for h in holders {
                hub.queue_note(h, format!("[Planetarium] Message from the user about {file}: \"{note}\""));
            }
        }
        (text, requester.map(|r| r.0))
    };
    match (decided, requester) {
        (Some(text), Some(key)) => {
            server::deliver_talk(&app, id, &key, text);
            server::publish(&app);
            true
        }
        _ => false,
    }
}

#[tauri::command]
fn get_usage() -> Option<usage::Usage> {
    server::current_usage()
}

/// "Wind down all agents": every agent in the middle of a turn gets `message` on its next step.
#[tauri::command]
fn wind_down(app: AppHandle, state: State<'_, AppState>, message: String) -> usize {
    let message = message.trim().chars().take(4000).collect::<String>();
    if message.is_empty() {
        return 0;
    }
    let n = state.agents.lock().unwrap().wind_down(&message, scanner::now_ms());
    server::publish(&app);
    n
}

#[tauri::command]
fn get_coord_settings(state: State<'_, AppState>) -> settings::CoordSettings {
    state.settings.lock().unwrap().value.clone()
}

#[tauri::command]
fn set_coord_settings(state: State<'_, AppState>, value: settings::CoordSettings) -> Result<settings::CoordSettings, String> {
    state.settings.lock().unwrap().set(value)
}

#[tauri::command]
fn reveal(state: State<'_, AppState>, id: String, rel_path: String, agent_key: Option<String>) -> bool {
    let Some(repo) = state.store.lock().unwrap().get(&id) else { return false };
    let mut root = PathBuf::from(&repo.path);
    // An agent in a git worktree works on its own copy: show that one.
    let worktree = agent_key.and_then(|k| {
        let hub = state.agents.lock().unwrap();
        let a = hub.get(&k)?;
        a.worktree.as_ref()?;
        a.tree_path.clone()
    });
    if let Some(tree) = worktree {
        let tree = PathBuf::from(tree);
        if let Some(main) = git::find(&tree).and_then(|c| c.main_root) {
            if let Some(r) = git::rebase(&repo.path, &main, &tree) {
                root = PathBuf::from(r);
            }
        }
    }
    let target = if rel_path.is_empty() { root.clone() } else { root.join(&rel_path) };
    // Only reveal things inside the repo.
    if !target.starts_with(&root) || !target.exists() {
        return false;
    }
    reveal_in_file_manager(&target)
}

#[cfg(windows)]
fn reveal_in_file_manager(path: &std::path::Path) -> bool {
    let native = path.to_string_lossy().replace('/', "\\");
    std::process::Command::new("explorer").arg(format!("/select,{native}")).spawn().is_ok()
}

#[cfg(target_os = "macos")]
fn reveal_in_file_manager(path: &std::path::Path) -> bool {
    std::process::Command::new("open").arg("-R").arg(path).spawn().is_ok()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn reveal_in_file_manager(path: &std::path::Path) -> bool {
    let dir = if path.is_dir() { path.to_path_buf() } else { path.parent().unwrap_or(path).to_path_buf() };
    std::process::Command::new("xdg-open").arg(dir).spawn().is_ok()
}

// ── Window + tray ─────────────────────────────────────────────────────────────

pub(crate) fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
    let _ = app.emit("window:visible", true);
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show Planetarium", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Planetarium", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut tray = TrayIconBuilder::with_id("planetarium")
        .tooltip("Planetarium")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        // Must be registered first: a second launch just brings the existing window forward.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main_window(app)))
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            // The Electron version keeps its list in %APPDATA%\Planetarium\repos.json; import it once.
            let electron_list = std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Planetarium").join("repos.json"));
            let store = RepoStore::open(data_dir.join("repos.json"), electron_list);

            let (tx, rx) = channel::<String>();
            let state = AppState {
                store: Mutex::new(store),
                agents: Mutex::new(agents::AgentHub::default()),
                settings: Mutex::new(settings::SettingsStore::open(data_dir.join("coordination.json"))),
                pending: Mutex::new(HashMap::new()),
                server_error: Mutex::new(None),
                watchers: Mutex::new(HashMap::new()),
                rescan_tx: Mutex::new(tx),
            };
            let repos = state.store.lock().unwrap().list();
            for repo in &repos {
                state.watch(repo);
            }
            app.manage(state);
            watch::spawn_rescan_worker(app.handle().clone(), rx);

            // Local endpoint for Claude Code's hooks (see server.rs).
            if let Err(e) = server::start(app.handle().clone()) {
                *app.state::<AppState>().server_error.lock().unwrap() = Some(e);
            }

            build_tray(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            match event {
                // Closing the window keeps Planetarium running in the tray. While it's there (or
                // minimized) the page stops drawing; agents are still tracked here in Rust.
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    let _ = window.hide();
                    let _ = window.emit("window:visible", false);
                }
                WindowEvent::Resized(_) => {
                    let minimized = window.is_minimized().unwrap_or(false);
                    let _ = window.emit("window:visible", !minimized);
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_repos, add_repo, remove_repo, scan_repo, reveal,
            list_agents, claude_status, claude_connect, claude_disconnect,
            list_collisions, resolve_collision, get_coord_settings, set_coord_settings,
            get_usage, wind_down
        ])
        .run(tauri::generate_context!())
        .expect("error while running Planetarium");
}
