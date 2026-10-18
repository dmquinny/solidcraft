//! The designs folder and its history: files on disk (what Syncthing and the user's own tools
//! see), and a git repository beside them (`<history>/designs.git`, the folder as its work
//! tree) that records every save, delete and restore with the user as author. Changes made
//! outside the app (a file copied in, or synced from another device) are committed as "disk"
//! the next time the service looks, so the history never misses a version.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use sha2::{Digest, Sha256};

/// Longest path (segments joined) and deepest folder the service accepts.
const MAX_PATH: usize = 400;
const MAX_DEPTH: usize = 8;
/// External changes are looked for at most this often.
const SWEEP_EVERY: Duration = Duration::from_secs(3);
/// Who gets the credit for changes made outside the app.
const DISK: &str = "disk";

pub struct Store {
    designs: PathBuf,
    git_dir: PathBuf,
    /// One git operation at a time (the index is shared).
    lock: Mutex<()>,
    last_sweep: Mutex<Option<Instant>>,
}

/// One entry of a folder listing.
pub struct Entry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
    pub mtime: SystemTime,
}

/// What the history knows about a file's current version.
#[derive(Clone, Default)]
pub struct Meta {
    pub etag: String,
    pub author: String,
    pub date: String,
}

/// One version of a design.
pub struct Version {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub message: String,
    pub size: u64,
}

/// One change in a folder's log.
pub struct Change {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub message: String,
    pub path: String,
}

pub enum Written {
    Created(String),
    Updated(String),
    Unchanged(String),
    /// Someone else saved since the version the client had: who and when.
    Stale(Meta),
}

/// A path under the designs folder, cleaned: no empty, hidden, `.` or `..` segments, no
/// backslashes or control characters.
pub fn clean_path(raw: &str) -> Result<String, String> {
    let mut segments = Vec::new();
    for s in raw.split('/') {
        if s.is_empty() {
            continue;
        }
        if s.starts_with('.') || s.contains('\\') || s.chars().any(char::is_control) || s.len() > 200 {
            return Err(format!("not a usable name: {s:?}"));
        }
        segments.push(s);
    }
    if segments.len() > MAX_DEPTH {
        return Err("the folder is nested too deep".into());
    }
    let path = segments.join("/");
    if path.len() > MAX_PATH {
        return Err("the path is too long".into());
    }
    Ok(path)
}

/// The strong validator of a version's bytes.
pub fn etag(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().take(10).map(|b| format!("{b:02x}")).collect()
}

fn io(path: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", path.display())
}

impl Store {
    /// Open (or make) the folder and its repository.
    pub fn open(designs: &Path, history: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(designs).map_err(|e| io(designs, e))?;
        std::fs::create_dir_all(history).map_err(|e| io(history, e))?;
        let store = Self { designs: designs.to_path_buf(), git_dir: history.join("designs.git"), lock: Mutex::new(()), last_sweep: Mutex::new(None) };
        if !store.git_dir.join("HEAD").is_file() {
            log::info!("making the history repository at {}", store.git_dir.display());
            store.git(&["init", "--initial-branch=main"])?;
        }
        // Syncthing's folder markers and other hidden files are not designs.
        let exclude = store.git_dir.join("info");
        std::fs::create_dir_all(&exclude).map_err(|e| io(&exclude, e))?;
        std::fs::write(exclude.join("exclude"), ".*\n").map_err(|e| io(&exclude, e))?;
        store.sweep(true);
        Ok(store)
    }

    /// Run git against the repository, with the designs folder as its work tree.
    fn git(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new("git")
            .arg("-c")
            .arg("safe.directory=*")
            .arg("-c")
            .arg("user.name=solidcraft-server")
            .arg("-c")
            .arg("user.email=server@solidcraft.local")
            .arg("-c")
            .arg("core.quotepath=off")
            .args(args)
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.designs)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C.UTF-8")
            .output()
            .map_err(|e| format!("git: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(format!("git {}: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    fn commit(&self, user: &str, message: &str, paths: &[&str]) -> Result<bool, String> {
        let mut add = vec!["add", "-A", "--"];
        add.extend(paths);
        self.git(&add)?;
        let staged = self.git(&["diff", "--cached", "--quiet"]).is_err();
        if !staged {
            return Ok(false);
        }
        let author = format!("{user} <{user}@solidcraft.local>");
        self.git(&["commit", "--quiet", "--no-verify", "--author", &author, "-m", message])?;
        Ok(true)
    }

    /// Commit whatever changed outside the app (at most every few seconds unless forced).
    pub fn sweep(&self, force: bool) {
        {
            let Ok(mut last) = self.last_sweep.lock() else { return };
            if !force && last.is_some_and(|t| t.elapsed() < SWEEP_EVERY) {
                return;
            }
            *last = Some(Instant::now());
        }
        let Ok(_guard) = self.lock.lock() else { return };
        match self.git(&["status", "--porcelain", "--untracked-files=all"]) {
            Ok(status) if status.trim().is_empty() => {}
            Ok(status) => {
                let n = status.lines().count();
                match self.commit(DISK, &format!("{n} change(s) outside the app"), &["."]) {
                    Ok(true) => log::info!("history: {n} change(s) made outside the app recorded"),
                    Ok(false) => {}
                    Err(e) => log::warn!("history: {e}"),
                }
            }
            Err(e) => log::warn!("history: {e}"),
        }
    }

    fn disk(&self, path: &str) -> PathBuf {
        let mut p = self.designs.clone();
        p.extend(path.split('/').filter(|s| !s.is_empty()));
        p
    }

    /// The entries of a folder (hidden names left out), `None` when there is no such folder.
    pub fn list(&self, folder: &str) -> Result<Option<Vec<Entry>>, String> {
        let dir = self.disk(folder);
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(&dir, e)),
        };
        let mut out = Vec::new();
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if !meta.is_dir() && !meta.is_file() {
                continue;
            }
            out.push(Entry { name, dir: meta.is_dir(), size: meta.len(), mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH) });
        }
        out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(Some(out))
    }

    /// The current bytes of a design and what the history knows about them.
    pub fn read(&self, path: &str) -> Result<Option<(Vec<u8>, Meta)>, String> {
        let file = self.disk(path);
        let bytes = match std::fs::read(&file) {
            Ok(b) => b,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::IsADirectory) => return Ok(None),
            Err(e) => return Err(io(&file, e)),
        };
        let mut meta = self.last_change(path).unwrap_or_default();
        meta.etag = etag(&bytes);
        Ok(Some((bytes, meta)))
    }

    /// Who last changed a path, and when (empty when the history has no record).
    fn last_change(&self, path: &str) -> Option<Meta> {
        let out = self.git(&["log", "-1", "--format=%an%x1f%aI", "--", path]).ok()?;
        let (author, date) = out.trim().split_once('\x1f')?;
        Some(Meta { etag: String::new(), author: author.to_string(), date: date.to_string() })
    }

    /// Write a design (folders made as needed). With `if_match`, only when the current version
    /// is still the one the client saw; with `only_new`, only when there is no such design yet.
    pub fn write(&self, path: &str, bytes: &[u8], user: &str, if_match: Option<&str>, only_new: bool) -> Result<Written, String> {
        let _guard = self.lock.lock().map_err(|_| "the store is poisoned".to_string())?;
        let file = self.disk(path);
        let existing = match std::fs::read(&file) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(io(&file, e)),
        };
        if only_new && existing.is_some() {
            let mut meta = self.last_change(path).unwrap_or_default();
            meta.etag = existing.as_deref().map(etag).unwrap_or_default();
            return Ok(Written::Stale(meta));
        }
        if let Some(expected) = if_match {
            let current = existing.as_deref().map(etag).unwrap_or_default();
            if current != expected {
                let mut meta = self.last_change(path).unwrap_or_default();
                meta.etag = current;
                return Ok(Written::Stale(meta));
            }
        }
        let tag = etag(bytes);
        if existing.as_deref() == Some(bytes) {
            return Ok(Written::Unchanged(tag));
        }
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        }
        let tmp = file.with_extension("part");
        std::fs::write(&tmp, bytes).map_err(|e| io(&tmp, e))?;
        std::fs::rename(&tmp, &file).map_err(|e| io(&file, e))?;
        self.commit(user, &format!("saved {path}"), &[path])?;
        Ok(if existing.is_some() { Written::Updated(tag) } else { Written::Created(tag) })
    }

    /// Remove a design (`false` when there was none).
    pub fn delete(&self, path: &str, user: &str) -> Result<bool, String> {
        let _guard = self.lock.lock().map_err(|_| "the store is poisoned".to_string())?;
        let file = self.disk(path);
        match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(io(&file, e)),
        }
        self.commit(user, &format!("deleted {path}"), &[path])?;
        Ok(true)
    }

    /// Every version of a design, newest first.
    pub fn history(&self, path: &str) -> Result<Vec<Version>, String> {
        let out = self.git(&["log", "--format=%H%x1f%an%x1f%aI%x1f%s", "--", path])?;
        let mut versions = Vec::new();
        for line in out.lines() {
            let mut f = line.split('\x1f');
            let (Some(hash), Some(author), Some(date), Some(message)) = (f.next(), f.next(), f.next(), f.next()) else { continue };
            let size = self.git(&["cat-file", "-s", &format!("{hash}:{path}")]).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            versions.push(Version { hash: hash.to_string(), author: author.to_string(), date: date.to_string(), message: message.to_string(), size });
        }
        Ok(versions)
    }

    /// The bytes of a design at a version (`None` when that version has no such file).
    pub fn version(&self, path: &str, hash: &str) -> Result<Option<Vec<u8>>, String> {
        if hash.is_empty() || hash.len() > 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("not a version".into());
        }
        let out = Command::new("git")
            .args(["-c", "safe.directory=*", "cat-file", "blob", &format!("{hash}:{path}")])
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_WORK_TREE", &self.designs)
            .output()
            .map_err(|e| format!("git: {e}"))?;
        Ok(out.status.success().then_some(out.stdout))
    }

    /// Make a version the current one again: a new commit, nothing rewritten.
    pub fn restore(&self, path: &str, hash: &str, user: &str) -> Result<Option<String>, String> {
        let Some(bytes) = self.version(path, hash)? else { return Ok(None) };
        let short: String = hash.chars().take(8).collect();
        let _guard = self.lock.lock().map_err(|_| "the store is poisoned".to_string())?;
        let file = self.disk(path);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        }
        let tmp = file.with_extension("part");
        std::fs::write(&tmp, &bytes).map_err(|e| io(&tmp, e))?;
        std::fs::rename(&tmp, &file).map_err(|e| io(&file, e))?;
        self.commit(user, &format!("restored {path} to {short}"), &[path])?;
        Ok(Some(etag(&bytes)))
    }

    /// The last `n` changes under a folder: one line per file touched.
    pub fn log(&self, folder: &str, n: usize) -> Result<Vec<Change>, String> {
        let count = format!("-{}", n.clamp(1, 200));
        let mut args = vec!["log", &count, "--name-only", "--format=%x1e%H%x1f%an%x1f%aI%x1f%s"];
        let scope = if folder.is_empty() { "." } else { folder };
        args.extend(["--", scope]);
        let out = self.git(&args)?;
        let mut changes = Vec::new();
        for record in out.split('\x1e').skip(1) {
            let mut lines = record.lines();
            let Some(head) = lines.next() else { continue };
            let mut f = head.split('\x1f');
            let (Some(hash), Some(author), Some(date), Some(message)) = (f.next(), f.next(), f.next(), f.next()) else { continue };
            for path in lines.map(str::trim).filter(|l| !l.is_empty()) {
                changes.push(Change {
                    hash: hash.to_string(),
                    author: author.to_string(),
                    date: date.to_string(),
                    message: message.to_string(),
                    path: path.to_string(),
                });
            }
        }
        changes.truncate(n);
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_cleaned() {
        assert_eq!(clean_path("/projects//bracket/part.solidcraft").as_deref(), Ok("projects/bracket/part.solidcraft"));
        assert_eq!(clean_path("").as_deref(), Ok(""));
        assert!(clean_path("../etc/passwd").is_err());
        assert!(clean_path("a/./b").is_err());
        assert!(clean_path(".git/config").is_err());
        assert!(clean_path("a\\b").is_err());
    }

    #[test]
    fn etags_follow_the_bytes() {
        assert_eq!(etag(b"a"), etag(b"a"));
        assert_ne!(etag(b"a"), etag(b"b"));
        assert_eq!(etag(b"").len(), 20);
    }
}
