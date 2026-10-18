//! Hooks for a host that shares a session's edits with other sessions as they happen (the web
//! app's live collaboration; a self-hosting patch, not upstream). Every top-level command that
//! changed the design of a [`Session`](crate::Session) marked `shared` is recorded here for the
//! host to send on; a command the host runs on another session's behalf goes through [`apply`]
//! so it is not recorded again. A design's [`fingerprint`] tells hosts whether two sessions
//! agree.
//!
//! Commands that move designs between files and sessions are never shared: `doc.*` (open,
//! recover) and `file.*` (new, save, export, insert from a local file) act on this host's
//! files, and `edit.checkpoint` / `edit.restore_checkpoint` name states of this session only.

use std::cell::{Cell, RefCell};

use serde_json::Value;
use solidcraft_doc::Document;

/// One command a shared session ran: the engine's JSON form, as the caller gave it.
#[derive(Clone, Debug)]
pub struct Executed {
    pub id: String,
    pub params: Value,
    /// One frame of an interactive gesture (`live: true`): hosts may coalesce these.
    pub live: bool,
}

/// Commands kept back from other sessions (prefixes).
const UNSHARED: &[&str] = &["doc.", "file.", "edit.checkpoint", "edit.restore_checkpoint"];
/// Commands kept while no host takes them (a host that never asks loses the oldest).
const MAX_KEPT: usize = 10_000;

thread_local! {
    static EXECUTED: RefCell<Vec<Executed>> = const { RefCell::new(Vec::new()) };
    static APPLYING: Cell<bool> = const { Cell::new(false) };
}

/// Would this command be shared?
pub fn shareable(id: &str) -> bool {
    !UNSHARED.iter().any(|p| id.starts_with(p))
}

pub(crate) fn record(id: &str, params: &Value, live: bool) {
    if !shareable(id) {
        return;
    }
    EXECUTED.with(|e| {
        let mut e = e.borrow_mut();
        if e.len() >= MAX_KEPT {
            e.remove(0);
        }
        e.push(Executed { id: id.to_string(), params: params.clone(), live });
    });
}

/// The commands shared sessions ran since the last call, in order.
pub fn take_executed() -> Vec<Executed> {
    EXECUTED.with(|e| std::mem::take(&mut *e.borrow_mut()))
}

/// Is a command from another session being applied right now?
pub fn applying() -> bool {
    APPLYING.with(Cell::get)
}

/// Run `f` (commands from another session) without recording what it runs.
pub fn apply<T>(f: impl FnOnce() -> T) -> T {
    let was = APPLYING.with(|a| a.replace(true));
    let r = f();
    APPLYING.with(|a| a.set(was));
    r
}

/// A short digest of a design's content: the same in two sessions that hold the same design
/// (of the same build), different otherwise.
pub fn fingerprint(doc: &Document) -> String {
    use std::hash::{Hash, Hasher};
    let bytes = serde_json::to_vec(doc).unwrap_or_default();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_sessions_record_changes_only() {
        let _ = take_executed();
        let mut s = crate::Session::default();
        s.shared = true;
        s.execute("parameters.add", &serde_json::json!({"name": "w", "expression": "10 mm"})).ok();
        s.execute("parameters.list", &Value::Null).ok();
        let got = take_executed();
        assert_eq!(got.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["parameters.add"]);
        // Applying another session's command records nothing.
        apply(|| s.execute("parameters.add", &serde_json::json!({"name": "h", "expression": "5 mm"})).ok());
        assert!(take_executed().is_empty());
        // A session that is not shared records nothing either.
        s.shared = false;
        s.execute("parameters.add", &serde_json::json!({"name": "d", "expression": "1 mm"})).ok();
        assert!(take_executed().is_empty());
        assert!(!shareable("doc.open") && !shareable("file.save") && shareable("solid.extrude"));
    }

    #[test]
    fn fingerprints_follow_the_design() {
        let a = crate::Session::default();
        let mut b = crate::Session::default();
        assert_eq!(fingerprint(&a.doc), fingerprint(&b.doc));
        b.execute("parameters.add", &serde_json::json!({"name": "w", "expression": "10 mm"})).ok();
        assert_ne!(fingerprint(&a.doc), fingerprint(&b.doc));
    }
}
