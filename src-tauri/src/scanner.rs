//! scanner.rs — turn a folder into a list of repo-relative file paths.
//!
//! Same rules as the Electron version (electron/scanner.cjs): ask git first (`git ls-files`
//! respects .gitignore exactly); otherwise walk the folder, skipping well-known generated
//! folders, and hand any nested git repo back to git.

use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Generated / vendored folders skipped by the fallback walker.
pub const IGNORED_DIRS: &[&str] = &[
    ".git", "node_modules", "bin", "obj", "dist", "target", ".vs", ".idea", ".angular", ".next", ".nuxt",
    ".cache", "coverage", "__pycache__", ".venv", "venv", ".gradle", ".turbo", ".parcel-cache",
    "DerivedData", "Pods", ".svelte-kit", ".pytest_cache", ".mypy_cache", ".terraform",
];

pub const MAX_FILES: usize = 60_000;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub ok: bool,
    pub repo_id: String,
    pub files: Vec<String>,
    pub truncated: bool,
    pub method: String,
    pub git_roots: Vec<String>,
    pub scanned_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Run git without flashing a console window on Windows. Returns stdout on success.
fn run_git(args: &[&str], cwd: &Path) -> Option<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(cwd);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().ok()?;
    if out.status.success() { Some(out.stdout) } else { None }
}

fn is_git_work_tree(dir: &Path) -> bool {
    run_git(&["rev-parse", "--is-inside-work-tree"], dir)
        .map(|o| String::from_utf8_lossy(&o).trim() == "true")
        .unwrap_or(false)
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Tracked + untracked-but-not-ignored files under `dir`, minus deleted ones, relative to `dir`.
fn git_list_files(dir: &Path) -> Option<Vec<String>> {
    let listed = run_git(&["-c", "core.quotepath=off", "ls-files", "-z", "--cached", "--others", "--exclude-standard"], dir)?;
    let deleted: HashSet<String> = run_git(&["-c", "core.quotepath=off", "ls-files", "-z", "--deleted"], dir)
        .map(|o| split_nul(&o).into_iter().collect())
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for f in split_nul(&listed) {
        // A trailing slash marks a nested checkout (e.g. a git worktree kept inside the repo),
        // not a file. Its own files belong to that checkout.
        if f.ends_with('/') || deleted.contains(&f) || !seen.insert(f.clone()) {
            continue;
        }
        files.push(f);
    }
    Some(files)
}

struct Walker {
    files: Vec<String>,
    git_roots: Vec<String>,
    truncated: bool,
}

impl Walker {
    fn walk(&mut self, abs: &Path, rel: &str) {
        if self.truncated {
            return;
        }
        if !rel.is_empty() && abs.join(".git").exists() {
            if let Some(git_files) = git_list_files(abs) {
                self.git_roots.push(rel.to_string());
                for f in git_files {
                    if self.files.len() >= MAX_FILES {
                        self.truncated = true;
                        return;
                    }
                    self.files.push(format!("{rel}/{f}"));
                }
                return;
            }
        }
        let Ok(read) = fs::read_dir(abs) else { return };
        let mut entries: Vec<_> = read.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            if self.truncated {
                return;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            let child_rel = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            // file_type() does not follow symlinks, so links are skipped (no cycles).
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if IGNORED_DIRS.contains(&name.as_str()) {
                    continue;
                }
                self.walk(&e.path(), &child_rel);
            } else if ft.is_file() {
                if self.files.len() >= MAX_FILES {
                    self.truncated = true;
                    return;
                }
                self.files.push(child_rel);
            }
        }
    }
}

/// Scan a repo folder. Never panics; problems come back as `ok: false` with a message.
pub fn scan_repo(repo_id: &str, root: &Path) -> ScanResult {
    let mut result = ScanResult {
        ok: true,
        repo_id: repo_id.to_string(),
        files: Vec::new(),
        truncated: false,
        method: "walk".into(),
        git_roots: Vec::new(),
        scanned_at: now_ms(),
        error: None,
    };
    if !root.is_dir() {
        result.ok = false;
        result.error = Some(format!("Folder not found: {}", root.display()));
        return result;
    }

    if is_git_work_tree(root) {
        if let Some(mut files) = git_list_files(root) {
            result.truncated = files.len() > MAX_FILES;
            files.truncate(MAX_FILES);
            result.files = files;
            result.method = "git".into();
            result.git_roots = vec![String::new()];
            return result;
        }
    }

    let mut w = Walker { files: Vec::new(), git_roots: Vec::new(), truncated: false };
    w.walk(root, "");
    result.files = w.files;
    result.git_roots = w.git_roots;
    result.truncated = w.truncated;
    result
}

/// Should a change at this repo-relative path trigger a rescan? (Ignore churn in .git, node_modules…)
pub fn is_interesting_change(rel: &Path) -> bool {
    !rel.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        IGNORED_DIRS.contains(&s.as_ref())
    })
}
