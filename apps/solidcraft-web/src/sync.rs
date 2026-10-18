//! Live collaboration on a server design (a self-hosting patch, not upstream). The commands run
//! here go to the storage service's log for the design (`POST files/<name>?ops`), the ones other
//! people ran come back (`GET ?ops&since=`) and are applied as if run here. Everyone replays the
//! same log in the server's order, so the sessions agree; a session that stops agreeing (a
//! command that fails here, or a fingerprint that differs from the one the other side reported)
//! starts again from the design's last saved version and replays the log from there.
//!
//! The engine hands over the commands (`solidcraft_engine::sync`): the session shown is marked
//! `shared` every frame, and commands applied on others' behalf run under `apply` so they are
//! not sent back. Only a design that came from the server (or was saved there) is shared: the
//! position in its log that its bytes correspond to came with them (files.rs keeps it).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use serde_json::{Value, json};
use solidcraft_engine::io::vfs;
use solidcraft_engine::sync as engine;
use solidcraft_ui_egui::SolveApp;
use wasm_bindgen_futures::spawn_local;

use crate::files::{self, Files};

/// Pending commands go out at most this often.
const SEND_EVERY_MS: f64 = 250.0;
/// How long one poll waits at the server for news.
const POLL_WAIT_S: u32 = 20;
/// After a failed request, before the next try.
const RETRY_MS: i32 = 3000;

/// The design being shared.
struct Shared {
    /// Its stored name (within the current server folder).
    name: String,
    /// The log position this session's state corresponds to.
    seq: u64,
    /// The latest saved marker known: its position and commit (where a fresh start loads from).
    base: (u64, String),
    /// Ends the poll loop of an earlier design.
    generation: u64,
    /// A fresh start from the base is wanted (a command failed, or the fingerprints differ).
    stale: bool,
}

#[derive(Default)]
pub struct Sync {
    shared: RefCell<Option<Shared>>,
    /// Commands run here, not sent yet (`{id, params, live, after}`).
    pending: RefCell<Vec<Value>>,
    sending: Cell<bool>,
    last_send: Cell<f64>,
    /// Entries from the server, in order, not applied yet.
    incoming: RefCell<Vec<Value>>,
    /// The latest position received from the server (polls continue from here).
    received: Cell<u64>,
    /// A fresh start, fetched: the base version's bytes and the log after it.
    fresh: RefCell<Option<(Vec<u8>, Vec<Value>)>>,
    fetching_fresh: Cell<bool>,
    /// Who has this design open, as the server sees it (including us).
    pub present: RefCell<Vec<String>>,
    /// The design's fingerprint, by the document it was taken from (serialising is not free).
    fingerprint: RefCell<Option<(usize, String)>>,
    /// This browser's id in the log: its own entries come back and are skipped.
    client: String,
}

impl Sync {
    pub fn new() -> Rc<Self> {
        let client = format!("{:x}{:x}", js_sys::Date::now() as u64, (js_sys::Math::random() * 1e12) as u64);
        Rc::new(Self { client, ..Default::default() })
    }

    /// Who else is here.
    pub fn others(&self) -> Vec<String> {
        let me = files::remote_user();
        self.present.borrow().iter().filter(|u| **u != me).cloned().collect()
    }

    fn fingerprint_of(&self, app: &SolveApp) -> String {
        let key = std::sync::Arc::as_ptr(&app.session.doc) as usize;
        if let Some((k, fp)) = self.fingerprint.borrow().as_ref()
            && *k == key
        {
            return fp.clone();
        }
        let fp = engine::fingerprint(&app.session.doc);
        *self.fingerprint.borrow_mut() = Some((key, fp.clone()));
        fp
    }

    /// Once a frame, after the app ran its commands: share the open server design, send what ran
    /// here, apply what came in.
    pub fn tick(self: &Rc<Self>, app: &mut SolveApp, files: &Rc<Files>) {
        let name = app.session.path.as_deref().filter(|_| files::remote_only()).and_then(|p| p.strip_prefix(files::OPFS)).map(str::to_string);
        self.follow(name.as_deref(), files);
        self.finish_fresh(app, files);
        let shared = self.shared.borrow().is_some();
        app.session.shared = shared;
        let ran = engine::take_executed();
        if !shared {
            return;
        }
        if let Some(name) = &name {
            files::set_fingerprint(name, &self.fingerprint_of(app));
        }
        if !ran.is_empty() {
            let after = self.fingerprint_of(app);
            let mut pending = self.pending.borrow_mut();
            let last = ran.len() - 1;
            for (i, e) in ran.into_iter().enumerate() {
                // One frame of a gesture replaces the frame before it.
                if e.live && pending.last().is_some_and(|p| p["live"] == true && p["id"] == e.id) {
                    pending.pop();
                }
                pending.push(json!({"id": e.id, "params": e.params, "live": e.live, "after": if i == last { Value::from(after.clone()) } else { Value::Null }}));
            }
        }
        self.send(files);
        self.apply(app, files);
        self.start_fresh(files);
    }

    /// Share `name` (or nothing): a change of design ends the old poll loop and starts a new one.
    fn follow(self: &Rc<Self>, name: Option<&str>, files: &Rc<Files>) {
        let current = self.shared.borrow().as_ref().map(|s| s.name.clone());
        if current.as_deref() == name {
            return;
        }
        let generation = self.shared.borrow().as_ref().map_or(0, |s| s.generation) + 1;
        self.pending.borrow_mut().clear();
        self.incoming.borrow_mut().clear();
        *self.fresh.borrow_mut() = None;
        self.present.borrow_mut().clear();
        *self.shared.borrow_mut() = None;
        let Some(name) = name else { return };
        // A design that never came from the server (new, unsaved) is shared once it is saved.
        let Some(base) = files::base_of(name) else { return };
        self.received.set(base.0);
        *self.shared.borrow_mut() = Some(Shared { name: name.to_string(), seq: base.0, base, generation, stale: false });
        self.poll(name.to_string(), generation, files.clone());
    }

    fn poll(self: &Rc<Self>, name: String, generation: u64, files: Rc<Files>) {
        let me = self.clone();
        spawn_local(async move {
            loop {
                if me.shared.borrow().as_ref().is_none_or(|s| s.generation != generation) {
                    return;
                }
                let url = format!("{}?ops&since={}&wait={POLL_WAIT_S}&client={}", files::remote_url(&name), me.received.get(), me.client);
                match files::remote_json("GET", &url, None).await {
                    Ok(v) => {
                        if me.shared.borrow().as_ref().is_none_or(|s| s.generation != generation) {
                            return;
                        }
                        if let Some(seq) = v["seq"].as_u64() {
                            me.received.set(me.received.get().max(seq));
                        }
                        if let Some(ops) = v["ops"].as_array() {
                            me.incoming.borrow_mut().extend(ops.iter().cloned());
                        }
                        let present: Vec<String> =
                            v["present"].as_array().map(|p| p.iter().filter_map(|u| u.as_str().map(str::to_string)).collect()).unwrap_or_default();
                        me.arrivals(&present, &files);
                        *me.present.borrow_mut() = present;
                        files.wake();
                    }
                    Err(e) => {
                        log::warn!("live: {e}");
                        files::sleep(RETRY_MS).await;
                    }
                }
            }
        });
    }

    /// Say who joined and who left.
    fn arrivals(&self, now: &[String], files: &Files) {
        let me = files::remote_user();
        let before = self.present.borrow();
        for u in now.iter().filter(|u| **u != me && !before.contains(u)) {
            files.say(format!("{u} is working on this design too"), false);
        }
        for u in before.iter().filter(|u| **u != me && !now.contains(u)) {
            files.say(format!("{u} left this design"), false);
        }
    }

    fn send(self: &Rc<Self>, files: &Rc<Files>) {
        let now = js_sys::Date::now();
        if self.sending.get() || self.pending.borrow().is_empty() || now - self.last_send.get() < SEND_EVERY_MS {
            return;
        }
        let Some((name, generation)) = self.shared.borrow().as_ref().map(|s| (s.name.clone(), s.generation)) else { return };
        let batch: Vec<Value> = std::mem::take(&mut *self.pending.borrow_mut());
        self.sending.set(true);
        self.last_send.set(now);
        let (me, files) = (self.clone(), files.clone());
        spawn_local(async move {
            let body = json!({"client": me.client, "ops": batch}).to_string();
            let r = files::remote_json("POST", &format!("{}?ops", files::remote_url(&name)), Some(body.as_bytes())).await;
            me.sending.set(false);
            match r {
                Ok(v) => {
                    if let Some(seq) = v["seq"].as_u64()
                        && let Some(s) = me.shared.borrow_mut().as_mut()
                        && s.generation == generation
                    {
                        s.seq = s.seq.max(seq);
                    }
                }
                Err(e) => {
                    // Kept for the next try, ahead of anything run since.
                    files.say(format!("live: {e}"), true);
                    if me.shared.borrow().as_ref().is_some_and(|s| s.generation == generation) {
                        let mut pending = me.pending.borrow_mut();
                        let later = std::mem::take(&mut *pending);
                        pending.extend(batch);
                        pending.extend(later);
                    }
                    me.last_send.set(js_sys::Date::now() + f64::from(RETRY_MS));
                }
            }
            files.wake();
        });
    }

    /// Apply the entries that came in: others' commands run here; a saved marker moves the base.
    fn apply(self: &Rc<Self>, app: &mut SolveApp, files: &Rc<Files>) {
        let entries: Vec<Value> = std::mem::take(&mut *self.incoming.borrow_mut());
        if entries.is_empty() {
            return;
        }
        let name = match self.shared.borrow().as_ref() {
            Some(s) => s.name.clone(),
            None => return,
        };
        let quiet = self.pending.borrow().is_empty() && !self.sending.get();
        let mut stale = false;
        let mut last_foreign_after: Option<String> = None;
        let mut seq = 0;
        let mut base = None;
        for e in &entries {
            seq = e["seq"].as_u64().unwrap_or(seq);
            if let Some(saved) = e.get("saved") {
                base = Some((seq, saved["commit"].as_str().unwrap_or("").to_string()));
                if let Some(tag) = saved["etag"].as_str() {
                    files::note_version(&name, tag);
                }
                // Someone's save is a checkpoint: a session that disagrees with it starts afresh.
                if let Some(fp) = saved["fp"].as_str()
                    && quiet
                    && e["client"].as_str() != Some(&self.client)
                    && e["user"].as_str() != Some(&files::remote_user())
                    && fp != self.fingerprint_of(app)
                {
                    stale = true;
                }
                last_foreign_after = None;
                continue;
            }
            if e["client"].as_str() == Some(&self.client) {
                last_foreign_after = None;
                continue;
            }
            let (Some(id), params) = (e["id"].as_str(), e["params"].clone()) else { continue };
            if let Err(err) = engine::apply(|| app.run(id, params)) {
                log::warn!("live: {id} from {}: {err}", e["user"].as_str().unwrap_or("?"));
                stale = true;
            }
            last_foreign_after = e["after"].as_str().map(str::to_string);
        }
        if let Some(fp) = last_foreign_after
            && quiet
            && !stale
            && fp != self.fingerprint_of(app)
        {
            log::warn!("live: this session disagrees with the others after {seq}");
            stale = true;
        }
        if let Some(s) = self.shared.borrow_mut().as_mut() {
            s.seq = s.seq.max(seq);
            if let Some(b) = base {
                s.base = b;
            }
            s.stale |= stale;
        }
        if stale {
            files.say("Catching up with the others' version of this design…", false);
        }
    }

    /// A fresh start: fetch the base version and the log after it, then (next frame) load the
    /// one and replay the other.
    fn start_fresh(self: &Rc<Self>, files: &Rc<Files>) {
        if self.fetching_fresh.get() {
            return;
        }
        let Some((name, base, generation)) =
            self.shared.borrow().as_ref().filter(|s| s.stale).map(|s| (s.name.clone(), s.base.clone(), s.generation))
        else {
            return;
        };
        self.fetching_fresh.set(true);
        let (me, files) = (self.clone(), files.clone());
        spawn_local(async move {
            let r = async {
                let bytes = files::remote_bytes(&format!("{}?at={}", files::remote_url(&name), base.1)).await?;
                let log = files::remote_json("GET", &format!("{}?ops&since={}&client={}", files::remote_url(&name), base.0, me.client), None).await?;
                Ok::<_, String>((bytes, log["ops"].as_array().cloned().unwrap_or_default()))
            };
            match r.await {
                Ok(fresh) if me.shared.borrow().as_ref().is_some_and(|s| s.generation == generation) => *me.fresh.borrow_mut() = Some(fresh),
                Ok(_) => {}
                Err(e) => files.say(format!("live: {e}"), true),
            }
            me.fetching_fresh.set(false);
            files.wake();
        });
    }

    /// Load the fetched base and replay the log after it (every command, ours included).
    fn finish_fresh(self: &Rc<Self>, app: &mut SolveApp, files: &Rc<Files>) {
        let Some((bytes, log)) = self.fresh.borrow_mut().take() else { return };
        let Some((name, base_seq)) = self.shared.borrow().as_ref().map(|s| (s.name.clone(), s.base.0)) else { return };
        let path = format!("{}{name}", files::OPFS);
        let pending: Vec<Value> = std::mem::take(&mut *self.pending.borrow_mut());
        let mut seq = base_seq;
        engine::apply(|| {
            vfs::insert(&path, bytes);
            app.open_path(&path);
            for e in &log {
                seq = e["seq"].as_u64().unwrap_or(seq);
                if e.get("saved").is_some() {
                    continue;
                }
                if let Some(id) = e["id"].as_str()
                    && let Err(err) = app.run(id, e["params"].clone())
                {
                    log::warn!("live: replaying {id}: {err}");
                }
            }
            // What ran here and was not sent yet comes after the log.
            for p in &pending {
                if let Some(id) = p["id"].as_str()
                    && app.run(id, p["params"].clone()).is_err()
                {
                    log::warn!("live: {id} no longer applies");
                }
            }
        });
        self.pending.borrow_mut().extend(pending);
        self.received.set(self.received.get().max(seq));
        if let Some(s) = self.shared.borrow_mut().as_mut() {
            s.seq = s.seq.max(seq);
            s.stale = false;
        }
        files.say("Caught up with the others' version of this design", false);
    }
}
