//! watch.rs — rescan a repo shortly after files in it change.
//!
//! Each repo gets a recursive file watcher. Watchers only report *which* repo changed; a single
//! worker thread waits until a repo has been quiet for a moment, rescans it, and sends the new
//! file list to the page as a `repos:updated` event.

use crate::scanner::{is_interesting_change, scan_repo};
use crate::store::RepoMeta;
use crate::AppState;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

const QUIET: Duration = Duration::from_millis(1200);

pub fn start_watcher(repo: &RepoMeta, tx: Sender<String>) -> Option<RecommendedWatcher> {
    let root = PathBuf::from(&repo.path);
    let id = repo.id.clone();
    let watch_root = root.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        let interesting = event.paths.iter().any(|p| match p.strip_prefix(&watch_root) {
            Ok(rel) => is_interesting_change(rel),
            Err(_) => true,
        });
        if interesting {
            let _ = tx.send(id.clone());
        }
    })
    .ok()?;
    watcher.watch(&root, RecursiveMode::Recursive).ok()?;
    Some(watcher)
}

/// The debounce-and-rescan worker. Runs for the life of the app.
pub fn spawn_rescan_worker(app: AppHandle, rx: Receiver<String>) {
    std::thread::spawn(move || {
        let mut pending: HashMap<String, Instant> = HashMap::new();
        loop {
            let wait = if pending.is_empty() { Duration::from_secs(3600) } else { Duration::from_millis(150) };
            match rx.recv_timeout(wait) {
                Ok(id) => {
                    pending.insert(id, Instant::now());
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            let now = Instant::now();
            let ready: Vec<String> = pending
                .iter()
                .filter(|(_, last)| now.duration_since(**last) >= QUIET)
                .map(|(id, _)| id.clone())
                .collect();
            for id in ready {
                pending.remove(&id);
                let repo = {
                    let state = app.state::<AppState>();
                    let store = state.store.lock().unwrap();
                    store.get(&id)
                };
                if let Some(repo) = repo {
                    let result = scan_repo(&repo.id, &PathBuf::from(&repo.path));
                    let _ = app.emit("repos:updated", result);
                }
            }
        }
    });
}
