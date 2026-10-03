//! store.rs — the repos you added, saved as JSON in the app's data folder
//! (on Windows: %APPDATA%\dev.planetarium.desktop\repos.json).
//!
//! Same file format as the Electron version, and on first launch the Electron version's list
//! (%APPDATA%\Planetarium\repos.json) is imported so both apps start with the same repos.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const PALETTE_SIZE: u32 = 10;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RepoMeta {
    pub id: String,
    pub path: String,
    pub name: String,
    pub color_index: u32,
    pub added_at: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct StoreFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    repos: Vec<RepoMeta>,
}

pub enum AddError {
    Missing(String),
    NotFolder(String),
    Duplicate { name: String, id: String },
    Io(String),
}

impl AddError {
    pub fn message(&self) -> String {
        match self {
            AddError::Missing(p) => format!("That folder doesn't exist: {p}"),
            AddError::NotFolder(p) => format!("That's a file, not a folder: {p}"),
            AddError::Duplicate { name, .. } => format!("{name} is already in the constellation."),
            AddError::Io(e) => format!("Couldn't save the repo list: {e}"),
        }
    }
}

pub struct RepoStore {
    file: PathBuf,
    repos: Vec<RepoMeta>,
}

/// 64-bit FNV-1a, hex. Stable ids for repo paths (case-insensitive on Windows).
fn id_for(path: &str) -> String {
    let key = if cfg!(windows) { path.to_lowercase() } else { path.to_string() };
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")[..12].to_string()
}

/// Absolute path without the `\\?\` prefix that `canonicalize` adds on Windows, no trailing slash.
pub fn normalize(p: &str) -> PathBuf {
    let trimmed = p.trim().trim_matches('"');
    let abs = std::path::absolute(trimmed).unwrap_or_else(|_| PathBuf::from(trimmed));
    let mut s = abs.to_string_lossy().into_owned();
    while s.len() > 3 && (s.ends_with('\\') || s.ends_with('/')) {
        s.pop();
    }
    PathBuf::from(s)
}

impl RepoStore {
    pub fn open(file: PathBuf, import_from: Option<PathBuf>) -> Self {
        let mut store = RepoStore { file, repos: Vec::new() };
        if store.file.exists() {
            store.repos = read_list(&store.file);
        } else if let Some(old) = import_from.filter(|p| p.exists()) {
            store.repos = read_list(&old);
            let _ = store.save();
        }
        store
    }

    fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let body = serde_json::to_string_pretty(&StoreFile { version: 1, repos: self.repos.clone() })
            .map_err(|e| e.to_string())?;
        let tmp = self.file.with_extension("json.tmp");
        fs::write(&tmp, body).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &self.file).map_err(|e| e.to_string())
    }

    pub fn list(&self) -> Vec<RepoMeta> {
        self.repos.clone()
    }

    pub fn get(&self, id: &str) -> Option<RepoMeta> {
        self.repos.iter().find(|r| r.id == id).cloned()
    }

    pub fn add(&mut self, folder: &str) -> Result<RepoMeta, AddError> {
        let path = normalize(folder);
        let shown = path.display().to_string();
        let meta = fs::metadata(&path).map_err(|_| AddError::Missing(shown.clone()))?;
        if !meta.is_dir() {
            return Err(AddError::NotFolder(shown));
        }
        let id = id_for(&shown);
        if let Some(existing) = self.repos.iter().find(|r| r.id == id || same_path(&r.path, &shown)) {
            return Err(AddError::Duplicate { name: existing.name.clone(), id: existing.id.clone() });
        }
        let used: Vec<u32> = self.repos.iter().map(|r| r.color_index).collect();
        let color_index = (0..PALETTE_SIZE)
            .find(|i| !used.contains(i))
            .unwrap_or(self.repos.len() as u32 % PALETTE_SIZE);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| shown.clone());
        let repo = RepoMeta { id, path: shown, name, color_index, added_at: crate::scanner::now_ms() };
        self.repos.push(repo.clone());
        self.save().map_err(AddError::Io)?;
        Ok(repo)
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.repos.len();
        self.repos.retain(|r| r.id != id);
        let changed = self.repos.len() != before;
        if changed {
            let _ = self.save();
        }
        changed
    }
}

fn same_path(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.eq_ignore_ascii_case(b) } else { a == b }
}

fn read_list(file: &Path) -> Vec<RepoMeta> {
    fs::read_to_string(file)
        .ok()
        .and_then(|s| serde_json::from_str::<StoreFile>(&s).ok())
        .map(|f| f.repos)
        .unwrap_or_default()
}
