//! Hooks for a host that shares a session's edits with other sessions as they happen (the web
//! app's live collaboration; a self-hosting patch, not upstream). Every top-level command that
//! changed the design of a [`Session`](crate::Session) marked `shared` is recorded here for the
//! host to send on; a command the host runs on another session's behalf goes through [`apply`]
//! so it is not recorded again. A design's [`fingerprint`] tells hosts whether two sessions
//! agree.
//!
//! Locks: the host tells this module who holds which component ([`set_locks`]; 0 is the design
//! as a whole). A command that would change a component someone else holds fails like any other
//! refused command, and the design stays as it was. [`touched`] says which components a change
//! reached.
//!
//! Commands that move designs between files and sessions are never shared: `doc.*` (open,
//! recover) and `file.*` (new, save, export, insert from a local file) act on this host's
//! files, and `edit.checkpoint` / `edit.restore_checkpoint` name states of this session only.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use serde_json::Value;
use solidcraft_doc::Document;

/// One command a shared session ran: the engine's JSON form, as the caller gave it.
#[derive(Clone, Debug)]
pub struct Executed {
    pub id: String,
    pub params: Value,
    /// One frame of an interactive gesture (`live: true`): hosts may coalesce these.
    pub live: bool,
    /// The components it changed (0 = the design as a whole), as [`touched`] tells them.
    pub components: Vec<u64>,
}

/// Commands kept back from other sessions (prefixes).
const UNSHARED: &[&str] = &["doc.", "file.", "edit.checkpoint", "edit.restore_checkpoint"];
/// Commands kept while no host takes them (a host that never asks loses the oldest).
const MAX_KEPT: usize = 10_000;

thread_local! {
    static EXECUTED: RefCell<Vec<Executed>> = const { RefCell::new(Vec::new()) };
    static APPLYING: Cell<bool> = const { Cell::new(false) };
    /// Who holds which component (0 = the whole design), and who this host is.
    static LOCKS: RefCell<(HashMap<u64, String>, String)> = RefCell::new((HashMap::new(), String::new()));
}

/// Would this command be shared?
pub fn shareable(id: &str) -> bool {
    !UNSHARED.iter().any(|p| id.starts_with(p))
}

pub(crate) fn record(id: &str, params: &Value, live: bool, components: Vec<u64>) {
    if !shareable(id) {
        return;
    }
    EXECUTED.with(|e| {
        let mut e = e.borrow_mut();
        if e.len() >= MAX_KEPT {
            e.remove(0);
        }
        e.push(Executed { id: id.to_string(), params: params.clone(), live, components });
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

/// Run `f` (commands from another session) without recording what it runs; locks don't apply.
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

/// Who holds which component (0 = the whole design), as the host last heard, and who the host
/// is: a change touching a component someone else holds is refused from now on.
pub fn set_locks(locks: impl IntoIterator<Item = (u64, String)>, me: &str) {
    LOCKS.with(|l| *l.borrow_mut() = (locks.into_iter().collect(), me.to_string()));
}

/// The components a change from `before` to `after` reached: those of the features added,
/// removed or edited, and 0 (the design as a whole) for parameters, components, placements,
/// joints, settings, the name or units. Empty for what locks leave alone (what is hidden,
/// render setups).
pub fn touched(before: &Document, after: &Document) -> Vec<u64> {
    let mut out = Vec::new();
    for f in &after.features {
        if before.features.iter().find(|b| b.id == f.id) != Some(f) {
            out.push(f.component);
        }
    }
    for f in &before.features {
        if !after.features.iter().any(|a| a.id == f.id) {
            out.push(f.component);
        }
    }
    let whole = before.params != after.params
        || before.components != after.components
        || before.occurrences != after.occurrences
        || before.assembly != after.assembly
        || before.sheet != after.sheet
        || before.plastic != after.plastic
        || before.parts != after.parts
        || before.name != after.name
        || before.units != after.units
        || before.metadata != after.metadata
        || before.marker != after.marker;
    if whole {
        out.push(0);
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// May this host change `touched`? Not when someone else holds one of them, or the whole design.
pub(crate) fn allowed(doc: &Document, touched: &[u64]) -> Result<(), String> {
    if touched.is_empty() {
        return Ok(());
    }
    LOCKS.with(|l| {
        let (locks, me) = &*l.borrow();
        if locks.is_empty() {
            return Ok(());
        }
        for c in touched.iter().chain(std::iter::once(&0)) {
            if let Some(holder) = locks.get(c)
                && holder != me
            {
                let what = match c {
                    0 => "the whole design".to_string(),
                    c => doc.components.iter().find(|k| k.id == *c).map_or_else(|| format!("component {c}"), |k| format!("component {}", k.name)),
                };
                return Err(format!("{what} is locked by {holder}"));
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_param(s: &mut crate::Session, name: &str) -> crate::Result<Value> {
        s.execute("parameters.add", &serde_json::json!({"name": name, "expression": "10 mm"}))
    }

    #[test]
    fn shared_sessions_record_changes_only() {
        let _ = take_executed();
        set_locks([], "");
        let mut s = crate::Session::default();
        s.shared = true;
        add_param(&mut s, "w").ok();
        s.execute("parameters.list", &Value::Null).ok();
        let got = take_executed();
        assert_eq!(got.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["parameters.add"]);
        assert_eq!(got[0].components, [0], "a parameter is a change to the design as a whole");
        // Applying another session's command records nothing.
        apply(|| add_param(&mut s, "h").ok());
        assert!(take_executed().is_empty());
        // A session that is not shared records nothing either.
        s.shared = false;
        add_param(&mut s, "d").ok();
        assert!(take_executed().is_empty());
        assert!(!shareable("doc.open") && !shareable("file.save") && shareable("solid.extrude"));
    }

    #[test]
    fn fingerprints_follow_the_design() {
        let a = crate::Session::default();
        let mut b = crate::Session::default();
        assert_eq!(fingerprint(&a.doc), fingerprint(&b.doc));
        add_param(&mut b, "w").ok();
        assert_ne!(fingerprint(&a.doc), fingerprint(&b.doc));
        assert_eq!(touched(&a.doc, &b.doc), [0]);
        assert!(touched(&a.doc, &a.doc).is_empty());
    }

    #[test]
    fn a_lock_held_by_someone_else_refuses_the_change() {
        let _ = take_executed();
        let mut s = crate::Session::default();
        s.shared = true;
        set_locks([(0, "felix".to_string())], "ben");
        let before = s.doc.clone();
        let r = add_param(&mut s, "w");
        assert!(matches!(r, Err(crate::EngineError::Disabled(_, ref why)) if why.contains("felix")), "{r:?}");
        assert!(std::sync::Arc::ptr_eq(&s.doc, &before) || *s.doc == *before, "the design stays as it was");
        assert!(take_executed().is_empty());
        // Our own lock, and no lock, let the change through; so does applying another session's command.
        set_locks([(0, "ben".to_string())], "ben");
        assert!(add_param(&mut s, "w").is_ok());
        set_locks([(0, "felix".to_string())], "ben");
        assert!(apply(|| add_param(&mut s, "h")).is_ok());
        set_locks([], "");
        let _ = take_executed();
    }
}
