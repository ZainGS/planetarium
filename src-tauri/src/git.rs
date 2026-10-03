//! git.rs — which git checkout (worktree) a path is in, and which branch that checkout is on.
//!
//! A git *worktree* is an extra working folder of the same repo with its own branch checked out
//! (`git worktree add ../salsa-feature feature`). Its files are separate copies on disk, so
//! agents in different worktrees can't overwrite each other.
//!
//! Planetarium shows one constellation per repo, so an agent in a worktree is placed on the
//! matching file of the repo you added (same repo-relative path), labelled with its branch.
//! Holds and collisions stay per checkout: two agents only collide when they're in the same one.
//!
//! This reads git's own files (.git, HEAD, commondir) instead of running git, so it's cheap
//! enough to do for every hook event, and results are cached briefly anyway.

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct Checkout {
    /// The checkout's top folder (where its `.git` is).
    pub root: PathBuf,
    /// For a linked worktree: the main checkout's folder. None for the main checkout itself
    /// (and for submodules or bare repos, which aren't treated as worktrees).
    pub main_root: Option<PathBuf>,
    /// Where this checkout's HEAD lives.
    pub head: PathBuf,
    /// Branch name, or "detached HEAD".
    pub branch: String,
}

pub const DETACHED: &str = "detached HEAD";

/// Resolve "." and ".." without touching the disk (canonicalize adds \\?\ on Windows).
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn read_branch(head: &Path) -> Option<String> {
    let text = fs::read_to_string(head).ok()?;
    let text = text.trim();
    if let Some(r) = text.strip_prefix("ref:") {
        let r = r.trim();
        Some(r.strip_prefix("refs/heads/").unwrap_or(r).to_string())
    } else if !text.is_empty() {
        // A commit id: detached. Not including the id keeps commits from looking like switches.
        Some(DETACHED.to_string())
    } else {
        None
    }
}

/// The checkout containing `path` (a file or folder; it doesn't have to exist yet).
pub fn find(path: &Path) -> Option<Checkout> {
    for dir in path.ancestors() {
        let dot = dir.join(".git");
        let Ok(meta) = fs::metadata(&dot) else { continue };
        let (git_dir, common) = if meta.is_dir() {
            (dot.clone(), dot.clone())
        } else {
            // "gitdir: <path>" (worktrees and submodules).
            let text = fs::read_to_string(&dot).ok()?;
            let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
            let git_dir = lexical(&dir.join(target));
            let common = match fs::read_to_string(git_dir.join("commondir")) {
                Ok(c) => lexical(&git_dir.join(c.trim())),
                Err(_) => git_dir.clone(),
            };
            (git_dir, common)
        };
        let head = git_dir.join("HEAD");
        let branch = read_branch(&head)?;
        let linked = git_dir != common;
        let main_root = if linked && common.file_name().map_or(false, |n| n == ".git") {
            common.parent().map(Path::to_path_buf)
        } else {
            None
        };
        return Some(Checkout { root: dir.to_path_buf(), main_root, head, branch });
    }
    None
}

fn norm(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Comparable form of a checkout folder (forward slashes; case-folded on Windows).
pub fn tree_key(root: &Path) -> String {
    norm(&root.to_string_lossy())
}

/// `path` moved from under `from` to the same place under `to`. None if it isn't under `from`.
pub fn rebase(path: &str, from: &Path, to: &Path) -> Option<String> {
    let p = path.replace('\\', "/");
    let f = from.to_string_lossy().replace('\\', "/");
    let f = f.trim_end_matches('/');
    let (pn, fnn) = (norm(&p), norm(f));
    let rest = if pn == fnn {
        ""
    } else if pn.starts_with(&fnn) && pn.as_bytes().get(fnn.len()) == Some(&b'/') {
        &p[f.len() + 1..]
    } else {
        return None;
    };
    let to_s = to.to_string_lossy().replace('\\', "/");
    let to_s = to_s.trim_end_matches('/');
    Some(if rest.is_empty() { to_s.to_string() } else { format!("{to_s}/{rest}") })
}

/// Short-lived cache of folder → checkout, so a burst of hook events doesn't re-read git files.
#[derive(Default)]
pub struct GitCache {
    entries: HashMap<PathBuf, (Option<Checkout>, u64)>,
}

const CACHE_MS: u64 = 2_000;

impl GitCache {
    pub fn checkout_for(&mut self, path: &str, now: u64) -> Option<Checkout> {
        let p = PathBuf::from(path);
        // Cache by folder: a file path's parent (the file itself may not exist yet).
        let dir = if p.is_dir() { p } else { p.parent().map(Path::to_path_buf).unwrap_or(p) };
        if let Some((c, at)) = self.entries.get(&dir) {
            if now.saturating_sub(*at) < CACHE_MS {
                return c.clone();
            }
        }
        let c = find(&dir);
        if self.entries.len() > 512 {
            self.entries.clear();
        }
        self.entries.insert(dir, (c.clone(), now));
        c
    }
}

/// Fill in which checkout and branch a hook event comes from, and move paths in a linked
/// worktree onto the main checkout's paths, so the agent lands on the right star.
pub fn annotate(ev: &mut crate::agents::HookEvent, cache: &mut GitCache, now: u64) {
    let from_path = ev.path.as_deref().and_then(|p| cache.checkout_for(p, now));
    let from_cwd = ev.cwd.as_deref().and_then(|c| cache.checkout_for(c, now));
    let Some(co) = from_path.clone().or(from_cwd.clone()) else { return };
    ev.tree = Some(tree_key(&co.root));
    ev.tree_path = Some(co.root.to_string_lossy().replace('\\', "/"));
    ev.branch = Some(co.branch.clone());
    ev.worktree = co.main_root.as_ref().and(co.root.file_name()).map(|n| n.to_string_lossy().into_owned());
    if let (Some(c), Some(p)) = (&from_path, ev.path.clone()) {
        if let Some(main) = &c.main_root {
            ev.path = rebase(&p, &c.root, main).or(Some(p));
        }
    }
    if let (Some(c), Some(d)) = (&from_cwd, ev.cwd.clone()) {
        if let Some(main) = &c.main_root {
            ev.cwd = rebase(&d, &c.root, main).or(Some(d));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").args(args).current_dir(dir).output().unwrap().status.success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn finds_main_checkout_and_worktrees() {
        let base = std::env::temp_dir().join(format!("pl-git-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let m = base.join("salsa");
        fs::create_dir_all(m.join("src")).unwrap();
        fs::write(m.join("src/view.ts"), "x").unwrap();
        git(&m, &["init", "-q", "-b", "main"]);
        git(&m, &["-c", "user.email=a@b", "-c", "user.name=a", "add", "."]);
        git(&m, &["-c", "user.email=a@b", "-c", "user.name=a", "commit", "-qm", "init"]);
        git(&m, &["worktree", "add", "-q", ".claude/worktrees/x", "-b", "feat-x"]);
        git(&m, &["worktree", "add", "-q", "../salsa-feature", "-b", "feature"]);

        let main = find(&m.join("src/view.ts")).unwrap();
        assert_eq!((main.root.clone(), main.main_root.clone(), main.branch.as_str()), (m.clone(), None, "main"));

        // A file that doesn't exist yet still resolves (Write creating a new file).
        let inner = find(&m.join(".claude/worktrees/x/src/new.ts")).unwrap();
        assert_eq!(inner.root, m.join(".claude/worktrees/x"));
        assert_eq!(inner.main_root.as_deref(), Some(m.as_path()));
        assert_eq!(inner.branch, "feat-x");

        let outer = find(&base.join("salsa-feature/src/view.ts")).unwrap();
        assert_eq!(outer.main_root.as_deref(), Some(m.as_path()));
        assert_eq!(outer.branch, "feature");

        // Paths move onto the main checkout; the event remembers which checkout it came from.
        let mut ev = crate::agents::HookEvent {
            event: "PreToolUse".into(),
            session_id: "s".into(),
            path: Some(base.join("salsa-feature/src/view.ts").to_string_lossy().into_owned()),
            cwd: Some(base.join("salsa-feature").to_string_lossy().into_owned()),
            ..Default::default()
        };
        annotate(&mut ev, &mut GitCache::default(), 1);
        assert_eq!(ev.path.as_deref(), Some(m.join("src/view.ts").to_string_lossy().as_ref()));
        assert_eq!(ev.cwd.as_deref(), Some(m.to_string_lossy().as_ref()));
        assert_eq!(ev.branch.as_deref(), Some("feature"));
        assert_eq!(ev.worktree.as_deref(), Some("salsa-feature"));
        assert_eq!(ev.tree.as_deref(), Some(tree_key(&base.join("salsa-feature")).as_str()));

        // Detached HEAD, and a branch switch.
        git(&base.join("salsa-feature"), &["checkout", "-q", "--detach"]);
        assert_eq!(find(&base.join("salsa-feature")).unwrap().branch, DETACHED);
        git(&m, &["checkout", "-q", "-b", "other"]);
        assert_eq!(read_branch(&main.head).as_deref(), Some("other"));

        // Not a git folder at all.
        let plain = base.join("plain");
        fs::create_dir_all(&plain).unwrap();
        assert!(find(&plain).is_none() || find(&plain).unwrap().root != plain);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn rebase_paths() {
        let r = rebase("C:\\w\\salsa-x\\src\\a.ts", Path::new("C:/w/salsa-x"), Path::new("C:/w/salsa"));
        assert_eq!(r.as_deref(), Some("C:/w/salsa/src/a.ts"));
        assert_eq!(rebase("/w/salsa-xy/a", Path::new("/w/salsa-x"), Path::new("/w/salsa")), None);
        assert_eq!(rebase("/w/salsa-x", Path::new("/w/salsa-x/"), Path::new("/w/salsa")).as_deref(), Some("/w/salsa"));
    }
}
