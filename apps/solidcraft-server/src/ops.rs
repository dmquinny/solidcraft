//! Live collaboration: one log of commands per design, in one order for everyone. Clients
//! append the commands they ran (`POST ?ops`) and fetch the ones they haven't seen (`GET
//! ?ops&since=`, waiting a while for new ones); each saved version of the design is a marker
//! in the same log, so a client that loads the file knows where in the log it stands. The log
//! lives in memory and is appended to `<history>/ops/<design>.jsonl`, so a restart keeps it.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Longest a client may wait for new commands in one request.
pub const MAX_WAIT: Duration = Duration::from_secs(25);
/// A client that hasn't asked for this long has left.
const PRESENT_FOR: Duration = Duration::from_secs(40);
/// Past this many entries, everything before the latest saved marker is dropped.
const MAX_ENTRIES: usize = 20_000;
/// Longest command list in one append, and longest single command (bytes of JSON).
const MAX_BATCH: usize = 500;
const MAX_COMMAND: usize = 1 << 20;

struct Log {
    entries: Vec<Value>,
    next: u64,
    /// Who is asking: client id → (user, last seen).
    present: HashMap<String, (String, Instant)>,
    file: PathBuf,
}

pub struct Ops {
    dir: PathBuf,
    logs: Mutex<HashMap<String, Log>>,
    changed: Condvar,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Ops {
    pub fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Self { dir: dir.to_path_buf(), logs: Mutex::new(HashMap::new()), changed: Condvar::new() })
    }

    /// The log of a design, loaded from disk the first time.
    fn log<'a>(&self, logs: &'a mut HashMap<String, Log>, path: &str) -> &'a mut Log {
        logs.entry(path.to_string()).or_insert_with(|| {
            let file = self.dir.join(format!("{}.jsonl", path.replace('/', "%2F")));
            let mut entries: Vec<Value> =
                std::fs::read_to_string(&file).map(|text| text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default();
            entries.retain(|e| e.get("seq").and_then(Value::as_u64).is_some());
            let next = entries.last().and_then(|e| e["seq"].as_u64()).unwrap_or(0) + 1;
            Log { entries, next, present: HashMap::new(), file }
        })
    }

    fn push(&self, log: &mut Log, mut entry: Value) -> u64 {
        let seq = log.next;
        log.next += 1;
        entry["seq"] = json!(seq);
        entry["t"] = json!(now_ms());
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log.file) {
            let _ = writeln!(f, "{entry}");
        }
        log.entries.push(entry);
        if log.entries.len() > MAX_ENTRIES
            && let Some(at) = log.entries.iter().rposition(|e| e.get("saved").is_some())
            && at > 0
        {
            log.entries.drain(..at);
            let text: String = log.entries.iter().map(|e| format!("{e}\n")).collect();
            let _ = std::fs::write(&log.file, text);
        }
        seq
    }

    /// A version of the design was saved (or found changed on disk): a marker every client
    /// takes as the state the following commands apply to. Returns its position.
    pub fn saved(&self, path: &str, etag: &str, commit: &str, user: &str, fingerprint: Option<&str>) -> u64 {
        let Ok(mut logs) = self.logs.lock() else { return 0 };
        let log = self.log(&mut logs, path);
        let seq = self.push(log, json!({"user": user, "saved": {"etag": etag, "commit": commit, "fp": fingerprint}}));
        self.changed.notify_all();
        seq
    }

    /// The latest saved marker of a design: (position, etag, commit).
    pub fn base(&self, path: &str) -> Option<(u64, String, String)> {
        let mut logs = self.logs.lock().ok()?;
        let log = self.log(&mut logs, path);
        let m = log.entries.iter().rev().find(|e| e.get("saved").is_some())?;
        Some((m["seq"].as_u64()?, m["saved"]["etag"].as_str()?.to_string(), m["saved"]["commit"].as_str().unwrap_or("").to_string()))
    }

    /// Commands a client ran, appended in order. Returns the last one's position.
    pub fn append(&self, path: &str, user: &str, client: &str, ops: &[Value]) -> Result<u64, String> {
        if ops.len() > MAX_BATCH {
            return Err(format!("at most {MAX_BATCH} commands at a time"));
        }
        let mut logs = self.logs.lock().map_err(|_| "the log is poisoned".to_string())?;
        let log = self.log(&mut logs, path);
        let mut last = log.next.saturating_sub(1);
        for op in ops {
            let Some(id) = op.get("id").and_then(Value::as_str) else { continue };
            if op.to_string().len() > MAX_COMMAND {
                return Err("a command is too large to share".into());
            }
            let entry = json!({
                "user": user,
                "client": client,
                "id": id,
                "params": op.get("params").cloned().unwrap_or(Value::Null),
                "live": op.get("live").and_then(Value::as_bool).unwrap_or(false),
                "after": op.get("after").cloned().unwrap_or(Value::Null),
            });
            last = self.push(log, entry);
        }
        self.changed.notify_all();
        Ok(last)
    }

    /// The entries after `since` (waiting up to `wait` for some to arrive), the latest position,
    /// the latest saved marker, and who else is here. `client` (id, user) marks the asker present.
    pub fn since(&self, path: &str, since: u64, wait: Duration, client: Option<(&str, &str)>) -> Value {
        let Ok(mut logs) = self.logs.lock() else { return json!({"error": "the log is poisoned"}) };
        let deadline = Instant::now() + wait.min(MAX_WAIT);
        loop {
            let log = self.log(&mut logs, path);
            if let Some((id, user)) = client {
                log.present.insert(id.to_string(), (user.to_string(), Instant::now()));
            }
            let latest = log.next.saturating_sub(1);
            if latest > since || Instant::now() >= deadline {
                log.present.retain(|_, (_, seen)| seen.elapsed() < PRESENT_FOR);
                let mut present: Vec<String> = log.present.values().map(|(u, _)| u.clone()).collect();
                present.sort();
                present.dedup();
                let ops: Vec<Value> = log.entries.iter().filter(|e| e["seq"].as_u64().is_some_and(|s| s > since)).cloned().collect();
                let base = log.entries.iter().rev().find(|e| e.get("saved").is_some()).map(|m| json!({"seq": m["seq"], "saved": m["saved"]}));
                return json!({"seq": latest, "base": base, "ops": ops, "present": present});
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let (guard, _) = match self.changed.wait_timeout(logs, left) {
                Ok(r) => r,
                Err(_) => return json!({"error": "the log is poisoned"}),
            };
            logs = guard;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_keeps_order_and_markers() {
        let dir = std::env::temp_dir().join(format!("solidcraft-ops-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ops = Ops::open(&dir).unwrap();
        let m = ops.saved("a/b.solidcraft", "e1", "c1", "ben", Some("fp0"));
        let last = ops.append("a/b.solidcraft", "ben", "k1", &[json!({"id": "x", "params": {"n": 1}, "after": "fp1"})]).unwrap();
        assert!(last > m);
        let r = ops.since("a/b.solidcraft", m, Duration::from_millis(0), Some(("k2", "felix")));
        assert_eq!(r["ops"].as_array().unwrap().len(), 1);
        assert_eq!(r["ops"][0]["id"], "x");
        assert_eq!(r["base"]["saved"]["etag"], "e1");
        assert_eq!(r["present"], json!(["felix"]));
        // A new Ops over the same folder reads the log back.
        let again = Ops::open(&dir).unwrap();
        assert_eq!(again.base("a/b.solidcraft").map(|b| b.0), Some(m));
        assert_eq!(again.since("a/b.solidcraft", 0, Duration::from_millis(0), None)["ops"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
