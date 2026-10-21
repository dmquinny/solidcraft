//! Files in the browser. Designs are kept in the origin private file system (OPFS): read into
//! the engine's in-memory files (`solidcraft_engine::io::vfs`) at start, written back whenever a
//! command writes one. Files come in from the computer through a file picker (the File System
//! Access API where the browser has it, an `<input type=file>` otherwise) or a drop on the page,
//! and go out as downloads (exports, and "Download design"). An autosave of unsaved work is kept
//! in OPFS too and offered again after a reload or a crash.
//!
//! Server storage (a self-hosting patch, not upstream): when the site has a `files/` endpoint
//! next to the app (nginx with WebDAV, packaging/docker/nginx.conf: `GET files/` lists the
//! folder as nginx's JSON autoindex, `GET files/<name>` reads, `PUT` writes, `DELETE` removes),
//! designs are kept there instead of in the browser, so every browser and device sees the same
//! files. The endpoint is probed once at start; without it (404, or anything but a JSON
//! listing) nothing changes. Designs can be filed in a subfolder of the server's folder (the
//! file panel's folder field, remembered per browser in local storage; nginx makes the folder on
//! the first save). The autosave stays in the browser: it is written on every edit. Material and
//! appearance libraries (`*.solidcraft-library.json`) in the server's `Libraries` folder are
//! fetched once at start, for the Appearance panel to offer.
//!
//! With the storage service (apps/solidcraft-server) behind `files/`, saves carry the version
//! they replace (`If-Match`): when someone else saved in between, the server answers 412, the
//! save is refused with their name, and the next save overwrites. The service also answers who
//! we are (`X-Remote-User`), a folder's recent changes (`?log`), a design's versions
//! (`?history`), any version's bytes (`?at=`) and a restore (`POST ?restore=`), which the
//! history window uses. Plain nginx storage answers none of that, and the app works as before.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Function, Object, Reflect, Uint8Array};
use serde_json::Value;
use solidcraft_engine::io::vfs;
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetFileOptions, FileSystemWritableFileStream};

/// Where OPFS files appear to the engine.
pub const OPFS: &str = "/opfs/";
/// Paths written here are handed to the user as downloads.
pub const DOWNLOADS: &str = "/downloads/";
/// Files brought in from the computer.
pub const UPLOAD: &str = "/upload/";
/// The autosave of unsaved work (in OPFS: `recovery.solidcraft` and `recovery.json`).
const RECOVERY: &str = "recovery.solidcraft";
/// The autosave earlier releases kept (compatibility shim, docs/compatibility.md): recovered
/// when there is no current one.
const LEGACY_RECOVERY: &str = "recovery.solvecraft";
const RECOVERY_META: &str = "recovery.json";

/// What to do with a file brought in from the computer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Intent {
    /// Open it (a design, or STEP/mesh as a new design).
    Open,
    /// Add its bodies to the open design (STEP, 3MF, STL).
    Insert,
}

/// A stored design, as the file panel lists it.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    /// Last written (ms since 1970; 0 when unknown).
    pub modified: f64,
}

/// One change on the server (a folder's log): who did what, when (ms since 1970).
#[derive(Clone, Debug)]
pub struct Change {
    pub author: String,
    pub date: f64,
    pub message: String,
    pub path: String,
}

/// Something another user shared with this one by name.
#[derive(Clone, Debug, PartialEq)]
pub struct Shared {
    /// A design's path, or a folder's ("" for the design folder itself).
    pub path: String,
    pub folder: bool,
    pub owner: String,
    pub edit: bool,
    /// When the design was last saved (ms since 1970; 0 when the server doesn't say).
    pub modified: f64,
}

/// One version of a server design.
#[derive(Clone, Debug)]
pub struct Version {
    pub hash: String,
    pub author: String,
    pub date: f64,
    pub message: String,
    pub size: u64,
}

/// What a sharing is about.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// A design of the current folder, by name.
    Design(String),
    /// The current folder itself.
    Folder,
}

/// The sharing of a design or folder, as the storage service tells it. `owner` is `None` when
/// it has no sharing of its own (then `inherited_from` says what it follows, if anything).
/// In `view` / `edit`, `None` means everyone.
#[derive(Clone, Debug, Default)]
pub struct Access {
    pub owner: Option<String>,
    pub view: Option<Vec<String>>,
    pub edit: Option<Vec<String>>,
    pub inherited_from: Option<String>,
    pub default_open: bool,
    pub can_manage: bool,
}

/// The site's users, as the storage service tells them.
#[derive(Clone, Debug, Default)]
pub struct Users {
    pub me: String,
    pub admin: bool,
    /// (name, admin).
    pub list: Vec<(String, bool)>,
}

/// State shared between the async browser calls and the app's frames.
#[derive(Default)]
pub struct Files {
    pub entries: RefCell<Vec<Entry>>,
    /// The subfolders of the server's current folder (server storage only): the panel offers them.
    pub folders: RefCell<Vec<String>>,
    /// The current folder's recent changes, newest first (the storage service only).
    pub changes: RefCell<Vec<Change>>,
    /// What others shared with this user by name (the storage service only).
    pub shared: RefCell<Vec<Shared>>,
    /// When the shared list was last asked for (ms since 1970): the start page asks again.
    pub shared_at: Cell<f64>,
    /// Who the storage service says we are (`?me`); None before it answered, and with plain
    /// nginx storage (no sign-in).
    pub me: RefCell<Option<Me>>,
    /// Shared designs whose file here was just replaced by a newer one: the start page makes
    /// their pictures again (`HomeState::forget`).
    pub repaint: RefCell<Vec<String>>,
    /// What this user may do in the current server folder, as its listing said: "edit", "view",
    /// "none" (not shared with them: only designs shared with them are listed) or "" (unknown).
    pub folder_access: RefCell<String>,
    /// A stored design to open as soon as the entries are loaded (after a folder change).
    pub open_next: RefCell<Option<String>>,
    /// The history window: a server design's name and its versions, newest first (empty while
    /// they are being fetched); `None` when it is closed.
    pub history: RefCell<Option<(String, Vec<Version>)>>,
    /// The users were asked for; what the server answered (the Users page).
    pub users_asked: Cell<bool>,
    pub users: RefCell<Option<Users>>,
    /// The Sharing window: what it is about (a design's name, or a folder: the current one), a
    /// label for it, and what the server said; `None` when closed.
    pub sharing: RefCell<Option<(Target, String, Option<Access>)>>,
    /// Files read from the computer: (name, bytes, what to do).
    pub incoming: RefCell<Vec<(String, Vec<u8>, Intent)>>,
    /// Messages for the status line (errors and confirmations).
    pub messages: RefCell<Vec<(String, bool)>>,
    /// The stored designs are loaded (the app may open them).
    pub ready: Cell<bool>,
    /// The file panel is open.
    pub panel: Cell<bool>,
    /// An autosave left by an earlier session: its design's name and file.
    pub recovery: RefCell<Option<(String, Option<String>)>>,
    /// The app's context, to wake it when a browser call finishes.
    pub ctx: RefCell<Option<egui::Context>>,
}

impl Files {
    pub(crate) fn say(&self, text: impl Into<String>, error: bool) {
        self.messages.borrow_mut().push((text.into(), error));
        self.wake();
    }

    /// A file arrived from the computer.
    fn arrive(&self, name: String, bytes: Vec<u8>, intent: Intent) {
        self.incoming.borrow_mut().push((name, bytes, intent));
        self.wake();
    }

    /// Run a frame (the app sleeps until something happens).
    pub(crate) fn wake(&self) {
        if let Some(c) = self.ctx.borrow().as_ref() {
            c.request_repaint();
        }
    }
}

thread_local! {
    /// The private file system isn't there (an insecure page: http on a network address, or an
    /// old browser): designs are kept in local storage instead.
    static LOCAL: Cell<bool> = const { Cell::new(false) };
}

/// Local storage keys of stored files.
const LOCAL_PREFIX: &str = "solidcraft.file.";
/// Local-storage files kept by earlier releases (compatibility shim, docs/compatibility.md):
/// still listed and opened; removing one removes either key.
const LEGACY_LOCAL_PREFIX: &str = "solvecraft.file.";

fn local() -> Result<web_sys::Storage, String> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten()).ok_or_else(|| "this browser keeps no local storage".to_string())
}

/// Text as is, anything else as hex (local storage holds strings).
fn encode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(t) => format!("t{t}"),
        Err(_) => {
            let mut s = String::with_capacity(bytes.len() * 2 + 1);
            s.push('x');
            for b in bytes {
                s.push_str(&format!("{b:02x}"));
            }
            s
        }
    }
}

fn decode(s: &str) -> Vec<u8> {
    if let Some(t) = s.strip_prefix('t') {
        return t.as_bytes().to_vec();
    }
    let h = s.strip_prefix('x').unwrap_or("");
    (0..h.len() / 2).filter_map(|i| h.get(2 * i..2 * i + 2).and_then(|x| u8::from_str_radix(x, 16).ok())).collect()
}

/// Is the browser's storage local storage (no OPFS here)?
pub fn local_only() -> bool {
    LOCAL.with(Cell::get)
}

thread_local! {
    /// The site has server storage (see the module doc): designs go there, not to the browser.
    static REMOTE: Cell<bool> = const { Cell::new(false) };
}

/// The signed-in user as the storage service sees them (`?me`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Me {
    pub name: String,
    pub admin: bool,
    /// Where Sign out goes (the proxy's sign-out page); None: no such page (a password sign-in).
    pub logout: Option<String>,
}

/// The server's design folder, relative to the page (same origin: no CORS, no configuration).
const REMOTE_DIR: &str = "files/";
/// Local storage key of the subfolder designs are filed in.
const REMOTE_FOLDER_KEY: &str = "solidcraft.remote_folder";

thread_local! {
    /// The subfolder of the server's design folder designs are filed in ("" is the folder itself,
    /// else a relative path like "projects/bracket"), remembered per browser.
    static REMOTE_FOLDER: RefCell<String> = const { RefCell::new(String::new()) };
    /// Who the server says we are (empty without the storage service).
    static REMOTE_USER: RefCell<String> = const { RefCell::new(String::new()) };
    /// The server version each design here came from, by name: `Some(etag)` is sent as
    /// `If-Match` on a save; `None` means the next save overwrites whatever is there (the server
    /// gave no ETag, or refused a save once); a name not here is saved as a new design only.
    static ETAGS: RefCell<HashMap<String, Option<String>>> = RefCell::new(HashMap::new());
    /// Where in a design's server log the bytes here stand (the version loaded or saved), by
    /// name: (position, commit). Live collaboration (sync.rs) starts from there.
    static BASES: RefCell<HashMap<String, (u64, String)>> = RefCell::new(HashMap::new());
    /// The open design's name and fingerprint, sent with its next save so the other sessions
    /// can tell whether they agree with it.
    static FINGERPRINT: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
    /// When each shared design's file here was saved on the server (`Shared::modified`), by
    /// path: a newer one is fetched again.
    static SHARED_STAMPS: RefCell<HashMap<String, f64>> = RefCell::new(HashMap::new());
}

/// The log position a server design's bytes here correspond to (sync.rs).
pub fn base_of(name: &str) -> Option<(u64, String)> {
    BASES.with(|b| b.borrow().get(name).cloned())
}

/// The server holds this version of the design now (its log said so): a save from here replaces
/// it instead of being refused.
pub fn note_version(name: &str, etag: &str) {
    ETAGS.with(|e| {
        e.borrow_mut().insert(name.to_string(), Some(etag.to_string()));
    });
}

/// The fingerprint of the design `name` holds here, to go with its next save.
pub fn set_fingerprint(name: &str, fingerprint: &str) {
    FINGERPRINT.with(|f| *f.borrow_mut() = Some((name.to_string(), fingerprint.to_string())));
}

/// The signed-in user (empty when the server doesn't say).
pub fn remote_user() -> String {
    REMOTE_USER.with(|u| u.borrow().clone())
}

/// Ask again what others shared with this user (the start page does, every half minute).
pub fn refresh_shared(files: &Rc<Files>) {
    if !remote_only() {
        return;
    }
    files.shared_at.set(js_sys::Date::now());
    let files = files.clone();
    spawn_local(async move {
        let shared = remote_shared().await;
        fetch_shared_files(&files, &shared);
        if *files.shared.borrow() != shared {
            *files.shared.borrow_mut() = shared;
            files.wake();
        }
    });
}

/// Go to the proxy's sign-out page; false when there is none (a password sign-in: closing the
/// browser ends it).
pub fn sign_out(files: &Files) -> bool {
    let Some(url) = files.me.borrow().as_ref().and_then(|m| m.logout.clone()) else { return false };
    web_sys::window().is_some_and(|w| w.location().assign(&url).is_ok())
}

/// Load the page again (a new build, or a sign-in that lapsed).
pub fn reload() {
    if let Some(w) = web_sys::window() {
        let _ = w.location().reload();
    }
}

/// "just now", "5 min ago", "3 h ago", "2 d ago", or the date.
pub fn ago(ms: f64) -> String {
    let s = ((js_sys::Date::now() - ms) / 1000.0).max(0.0);
    if s < 60.0 {
        "just now".into()
    } else if s < 3600.0 {
        format!("{} min ago", (s / 60.0) as u64)
    } else if s < 86_400.0 {
        format!("{} h ago", (s / 3600.0) as u64)
    } else if s < 30.0 * 86_400.0 {
        format!("{} d ago", (s / 86_400.0) as u64)
    } else {
        when(ms).chars().take(10).collect()
    }
}

/// A moment (ms since 1970) as the browser's local date and time.
pub fn when(ms: f64) -> String {
    String::from(js_sys::Date::new(&JsValue::from_f64(ms)).to_locale_string("default", &JsValue::UNDEFINED))
}

/// Are designs kept on the server?
pub fn remote_only() -> bool {
    REMOTE.with(Cell::get)
}

/// The server subfolder designs are filed in ("" is the design folder itself).
pub fn remote_folder() -> String {
    REMOTE_FOLDER.with(|f| f.borrow().clone())
}

/// `folder` as a server path: segments trimmed, empty ones dropped, none hidden or ".." (a
/// backslash counts as a slash).
fn clean_folder(folder: &str) -> Result<String, String> {
    let mut segments = Vec::new();
    for s in folder.split(['/', '\\']).map(str::trim).filter(|s| !s.is_empty()) {
        if s.starts_with('.') {
            return Err(format!("a folder name can't start with a dot ({s})"));
        }
        segments.push(s);
    }
    let folder = segments.join("/");
    if folder.len() > 200 {
        return Err("the folder path is too long".to_string());
    }
    Ok(folder)
}

/// The folder this browser remembers.
fn stored_folder() -> String {
    local().ok().and_then(|st| st.get_item(REMOTE_FOLDER_KEY).ok().flatten()).and_then(|f| clean_folder(&f).ok()).unwrap_or_default()
}

/// Files that stay in the browser even with server storage: the autosave is written on every edit.
fn stays_local(name: &str) -> bool {
    name == RECOVERY || name == RECOVERY_META || name == LEGACY_RECOVERY
}

/// The current folder's URL (relative to the page, trailing slash).
fn remote_dir() -> String {
    let folder = remote_folder();
    if folder.is_empty() {
        return REMOTE_DIR.to_string();
    }
    let path: Vec<String> = folder.split('/').map(|s| String::from(js_sys::encode_uri_component(s))).collect();
    format!("{REMOTE_DIR}{}/", path.join("/"))
}

pub(crate) fn remote_url(name: &str) -> String {
    format!("{}{}", remote_dir(), String::from(js_sys::encode_uri_component(name)))
}

/// Where the files of shared designs are kept for their pictures (not `OPFS`: saving never
/// goes there).
pub const SHARED: &str = "/shared/";

/// A design's URL from its path under the designs folder (any folder, each segment encoded).
fn remote_path_url(path: &str) -> String {
    let encoded: Vec<String> = path.split('/').map(|s| String::from(js_sys::encode_uri_component(s))).collect();
    format!("{REMOTE_DIR}{}", encoded.join("/"))
}

/// Fetch the shared designs' files into the engine's files under `SHARED`, so the start page
/// can show their pictures.
fn fetch_shared_files(files: &Rc<Files>, shared: &[Shared]) {
    for s in shared.iter().filter(|s| !s.folder) {
        let key = format!("{SHARED}{}", s.path);
        let known = SHARED_STAMPS.with(|m| m.borrow().get(&s.path).copied());
        if vfs::exists(&key) && known == Some(s.modified) {
            continue;
        }
        // Claimed now, so a slow fetch isn't started again by the next refresh.
        SHARED_STAMPS.with(|m| {
            m.borrow_mut().insert(s.path.clone(), s.modified);
        });
        let (files, path, stamp) = (files.clone(), s.path.clone(), s.modified);
        spawn_local(async move {
            match remote_bytes(&remote_path_url(&path)).await {
                Ok(bytes) => {
                    let replaced = vfs::exists(&key);
                    vfs::insert(&key, bytes);
                    if replaced {
                        files.repaint.borrow_mut().push(key);
                    }
                }
                Err(e) => {
                    log::warn!("{path}: {e}");
                    SHARED_STAMPS.with(|m| {
                        if m.borrow().get(&path) == Some(&stamp) {
                            m.borrow_mut().remove(&path);
                        }
                    });
                }
            }
            files.wake();
        });
    }
}

/// Where the libraries in the server's `Libraries` folder appear to the engine; the Appearance
/// panel lists them (`SERVER_LIBRARIES` in crates/ui-egui/src/dialogs_library.rs).
pub const LIBRARIES: &str = "/libraries/";
/// The folder, in the server's design folder, that material and appearance libraries are kept in.
const LIBRARY_FOLDER: &str = "Libraries";
const LIBRARY_SUFFIX: &str = ".solidcraft-library.json";

/// Fetch the material and appearance libraries (`*.solidcraft-library.json`) in the server's
/// `Libraries` folder into the engine's files under `LIBRARIES`, once at start. Without the
/// folder nothing happens.
fn fetch_libraries(files: &Rc<Files>) {
    let files = files.clone();
    spawn_local(async move {
        let Ok(r) = remote_request("GET", &format!("{REMOTE_DIR}{LIBRARY_FOLDER}/"), None, &[]).await else { return };
        if !is_listing(&r) {
            return;
        }
        let Ok(rows) = json_rows(r).await else { return };
        for row in rows.iter().take(64) {
            let Some(name) = row.get("name") else { continue };
            if row.get("type").is_some_and(|t| t == "directory") || name.starts_with('.') || !name.to_lowercase().ends_with(LIBRARY_SUFFIX) {
                continue;
            }
            match remote_bytes(&remote_path_url(&format!("{LIBRARY_FOLDER}/{name}"))).await {
                Ok(bytes) => vfs::insert(&format!("{LIBRARIES}{name}"), bytes),
                Err(e) => log::warn!("{LIBRARY_FOLDER}/{name}: {e}"),
            }
        }
        files.wake();
    });
}

/// Go to something another user shared: a folder is shown in the panel; a design's folder is
/// entered and the design opened once its entries are in (the host's frame does that).
pub fn open_shared(files: &Rc<Files>, shared: &Shared) {
    let (folder, design) = if shared.folder {
        (shared.path.clone(), None)
    } else {
        match shared.path.rsplit_once('/') {
            Some((dir, name)) => (dir.to_string(), Some(name.to_string())),
            None => (String::new(), Some(shared.path.clone())),
        }
    };
    if folder != remote_folder()
        && let Err(e) = set_remote_folder(files, &folder)
    {
        files.say(e, true);
        return;
    }
    match design {
        Some(name) => *files.open_next.borrow_mut() = Some(name),
        None => files.panel.set(true),
    }
    files.wake();
}

/// A JSON answer from the server (any status but 2xx is an error, with the server's words).
pub(crate) async fn remote_json(method: &str, url: &str, body: Option<&[u8]>) -> Result<serde_json::Value, String> {
    let headers: &[(&str, &str)] = if body.is_some() { &[("Content-Type", "application/json")] } else { &[] };
    let r = remote_request(method, url, body, headers).await?;
    let status = r.status();
    let text = JsFuture::from(r.text().map_err(err)?).await.map_err(err)?.as_string().unwrap_or_default();
    if !r.ok() {
        let why = text.trim();
        return Err(if why.is_empty() || why.starts_with('{') { format!("HTTP {status}") } else { format!("HTTP {status}: {why}") });
    }
    serde_json::from_str(&text).map_err(|e| format!("not JSON: {e}"))
}

fn access_url(target: &Target) -> String {
    match target {
        Target::Design(name) => format!("{}?access", remote_url(name)),
        Target::Folder => format!("{}?access", remote_dir()),
    }
}

/// Open the Sharing window on a design or the current folder and ask the server about it.
pub fn sharing(files: &Rc<Files>, target: Target) {
    let label = match &target {
        Target::Design(n) => n.clone(),
        Target::Folder => {
            let f = remote_folder();
            if f.is_empty() { "the whole design folder".to_string() } else { format!("the folder {f}") }
        }
    };
    *files.sharing.borrow_mut() = Some((target.clone(), label, None));
    if !files.users_asked.get() {
        users(files);
    }
    sharing_request(files, target, "GET", None);
}

/// Set who may view and who may edit (`None`: everyone); owners and admins only.
pub fn set_sharing(files: &Rc<Files>, target: Target, view: Option<Vec<String>>, edit: Option<Vec<String>>) {
    let body = serde_json::json!({"view": view, "edit": edit}).to_string();
    sharing_request(files, target, "POST", Some(body));
}

/// Drop a sharing of its own: it follows its folder (or the default) again.
pub fn clear_sharing(files: &Rc<Files>, target: Target) {
    sharing_request(files, target, "DELETE", None);
}

fn sharing_request(files: &Rc<Files>, target: Target, method: &'static str, body: Option<String>) {
    let files = files.clone();
    spawn_local(async move {
        match remote_json(method, &access_url(&target), body.as_deref().map(str::as_bytes)).await {
            Ok(v) => {
                let names =
                    |k: &str| v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect::<Vec<_>>());
                let access = Access {
                    owner: v["owner"].as_str().map(str::to_string),
                    view: names("view"),
                    edit: names("edit"),
                    inherited_from: v["inherited_from"].as_str().map(str::to_string),
                    default_open: v["default_open"].as_bool().unwrap_or(true),
                    can_manage: v["can_manage"].as_bool().unwrap_or(false),
                };
                if let Some((t, _, a)) = files.sharing.borrow_mut().as_mut()
                    && *t == target
                {
                    *a = Some(access);
                }
                if method != "GET" {
                    files.say("Sharing changed", false);
                }
            }
            Err(e) => files.say(format!("sharing: {e}"), true),
        }
        files.wake();
    });
}

/// Ask the server who is who (the Users page).
pub fn users(files: &Rc<Files>) {
    files.users_asked.set(true);
    users_request(files, "GET", &format!("{REMOTE_DIR}?users"), None, None);
}

/// Add a user, give one a new password, or change whether they are an admin (admins only).
pub fn set_user(files: &Rc<Files>, name: &str, password: Option<&str>, admin: Option<bool>) {
    let body = serde_json::json!({"name": name.trim(), "password": password, "admin": admin}).to_string();
    users_request(files, "POST", &format!("{REMOTE_DIR}?users"), Some(body), Some(format!("{}: done", name.trim())));
}

/// Remove a user (admins only).
pub fn remove_user(files: &Rc<Files>, name: &str) {
    let url = format!("{REMOTE_DIR}?users={}", String::from(js_sys::encode_uri_component(name)));
    users_request(files, "DELETE", &url, None, Some(format!("{name}: removed")));
}

/// Change this user's own password (the browser asks for it again on the next request).
pub fn set_password(files: &Rc<Files>, password: &str) {
    let body = serde_json::json!({"password": password}).to_string();
    users_request(
        files,
        "POST",
        &format!("{REMOTE_DIR}?password"),
        Some(body),
        Some("Your password is changed: the browser will ask for it again".to_string()),
    );
}

fn users_request(files: &Rc<Files>, method: &'static str, url: &str, body: Option<String>, done: Option<String>) {
    let (files, url) = (files.clone(), url.to_string());
    spawn_local(async move {
        match remote_json(method, &url, body.as_deref().map(str::as_bytes)).await {
            Ok(v) => {
                let list = v["users"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|u| Some((u["name"].as_str()?.to_string(), u["admin"].as_bool().unwrap_or(false)))).collect())
                    .unwrap_or_default();
                *files.users.borrow_mut() =
                    Some(Users { me: v["me"].as_str().unwrap_or_default().to_string(), admin: v["admin"].as_bool().unwrap_or(false), list });
                if let Some(d) = done {
                    files.say(d, false);
                }
            }
            Err(e) => files.say(format!("users: {e}"), true),
        }
        files.wake();
    });
}

/// Bytes from the server (any status but 2xx is an error).
pub(crate) async fn remote_bytes(url: &str) -> Result<Vec<u8>, String> {
    let r = remote_request("GET", url, None, &[]).await?;
    if !r.ok() {
        return Err(format!("HTTP {}", r.status()));
    }
    let buf = JsFuture::from(r.array_buffer().map_err(err)?).await.map_err(err)?;
    Ok(Uint8Array::new(&buf).to_vec())
}

/// Wait `ms` (the browser's timer).
pub(crate) async fn sleep(ms: i32) {
    let p = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(p).await;
}

/// One request to the server's design folder: the response, whatever its status.
async fn remote_request(method: &str, url: &str, body: Option<&[u8]>, headers: &[(&str, &str)]) -> Result<web_sys::Response, String> {
    let window = web_sys::window().ok_or("no window")?;
    let init = web_sys::RequestInit::new();
    init.set_method(method);
    init.set_cache(web_sys::RequestCache::NoStore);
    if let Some(b) = body {
        init.set_body(&Uint8Array::from(b));
    }
    if !headers.is_empty() {
        let h = web_sys::Headers::new().map_err(err)?;
        for (k, v) in headers {
            h.set(k, v).map_err(err)?;
        }
        init.set_headers(&h);
    }
    JsFuture::from(window.fetch_with_str_and_init(url, &init))
        .await
        .map_err(|e| format!("the server's design folder is unreachable ({})", err(e)))?
        .dyn_into()
        .map_err(|_| "no response".to_string())
}

/// A server folder's files (name, modified), its subfolders, and its recent changes.
#[derive(Default)]
struct Listing {
    files: Vec<(String, f64)>,
    folders: Vec<String>,
    changes: Vec<Change>,
    shared: Vec<Shared>,
    me: Option<Me>,
    /// `X-Folder-Access` ("" when the server doesn't say).
    access: String,
}

/// A JSON array response as its objects' string fields, by key.
async fn json_rows(r: web_sys::Response) -> Result<Vec<HashMap<String, String>>, String> {
    let list: Array = JsFuture::from(r.json().map_err(err)?).await.map_err(err)?.dyn_into().map_err(|_| "the answer isn't a list".to_string())?;
    let mut rows = Vec::new();
    for e in list.iter() {
        let mut row = HashMap::new();
        let obj: Object = e.clone().unchecked_into();
        for k in Object::keys(&obj).iter().filter_map(|k| k.as_string()) {
            if let Some(v) = Reflect::get(&e, &k.clone().into())
                .ok()
                .and_then(|v| v.as_string().or_else(|| v.as_f64().map(|n| n.to_string())).or_else(|| v.as_bool().map(|b| b.to_string())))
            {
                row.insert(k, v);
            }
        }
        rows.push(row);
    }
    Ok(rows)
}

fn date_of(row: &HashMap<String, String>, key: &str) -> f64 {
    row.get(key).map(|m| js_sys::Date::parse(m)).filter(|t| !t.is_nan()).unwrap_or(0.0)
}

/// Who the storage service says we are (None from plain nginx storage, or when nobody is).
async fn remote_me() -> Option<Me> {
    let r = remote_request("GET", &format!("{REMOTE_DIR}?me"), None, &[]).await.ok()?;
    if !is_listing(&r) {
        return None;
    }
    let text = JsFuture::from(r.text().ok()?).await.ok()?.as_string()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    // An older service answers the folder listing (an array) to any query it doesn't know.
    let name = v.as_object().and_then(|o| o.get("name")).and_then(Value::as_str).filter(|n| !n.is_empty())?;
    if v["anonymous"].as_bool().unwrap_or(false) {
        return None;
    }
    Some(Me {
        name: name.to_string(),
        admin: v["admin"].as_bool().unwrap_or(false),
        logout: v["logout"].as_str().filter(|u| u.starts_with('/') || u.starts_with("https://")).map(str::to_string),
    })
}

/// What others shared with this user (none from plain nginx storage).
async fn remote_shared() -> Vec<Shared> {
    let Ok(r) = remote_request("GET", &format!("{REMOTE_DIR}?shared"), None, &[]).await else { return Vec::new() };
    if !is_listing(&r) {
        return Vec::new();
    }
    let Ok(rows) = json_rows(r).await else { return Vec::new() };
    rows.iter()
        .filter_map(|row| {
            Some(Shared {
                path: row.get("path")?.clone(),
                folder: row.get("folder").is_some_and(|f| f == "true"),
                owner: row.get("owner").cloned().unwrap_or_default(),
                edit: row.get("edit").is_some_and(|e| e == "true"),
                modified: date_of(row, "modified"),
            })
        })
        .collect()
}

/// A folder's recent changes (none from plain nginx storage).
async fn remote_log(dir: &str) -> Vec<Change> {
    let Ok(r) = remote_request("GET", &format!("{dir}?log"), None, &[]).await else { return Vec::new() };
    if !is_listing(&r) {
        return Vec::new();
    }
    let Ok(rows) = json_rows(r).await else { return Vec::new() };
    rows.iter()
        .filter_map(|row| {
            let author = row.get("author")?.clone();
            Some(Change {
                author,
                date: date_of(row, "date"),
                message: row.get("message").cloned().unwrap_or_default(),
                path: row.get("path").cloned().unwrap_or_default(),
            })
        })
        .collect()
}

/// Does the response carry nginx's JSON listing?
fn is_listing(r: &web_sys::Response) -> bool {
    r.ok() && r.headers().get("content-type").ok().flatten().is_some_and(|t| t.starts_with("application/json"))
}

/// The server's current folder: `Some` listing when the site has server storage, `None` when it
/// has not (404, or no JSON listing: the upstream site), an error when it failed.
async fn remote_list() -> Result<Option<Listing>, String> {
    let dir = remote_dir();
    let r = remote_request("GET", &dir, None, &[]).await?;
    if r.status() == 404 && dir != REMOTE_DIR {
        // A subfolder the server hasn't got yet (nginx makes it on the first save) is empty, as
        // long as the site has server storage at all.
        return Ok(is_listing(&remote_request("GET", REMOTE_DIR, None, &[]).await?).then(Listing::default));
    }
    if r.status() == 404 || (r.ok() && !is_listing(&r)) {
        return Ok(None);
    }
    if !r.ok() {
        return Err(format!("the server's design folder answered HTTP {}", r.status()));
    }
    if let Some(user) = r.headers().get("x-remote-user").ok().flatten() {
        REMOTE_USER.with(|u| *u.borrow_mut() = user);
    }
    let mut listing = Listing { access: r.headers().get("x-folder-access").ok().flatten().unwrap_or_default(), ..Listing::default() };
    for row in json_rows(r).await? {
        let Some(name) = row.get("name").cloned() else { continue };
        if name.starts_with('.') || stays_local(&name) {
            continue;
        }
        match row.get("type").map(String::as_str) {
            Some("directory") => listing.folders.push(name),
            Some("file") | None => listing.files.push((name, date_of(&row, "mtime"))),
            _ => {}
        }
    }
    listing.folders.sort_by_key(|f| f.to_lowercase());
    listing.changes = remote_log(&dir).await;
    listing.shared = remote_shared().await;
    // An older service has no `?me`: then the listing's name is all we know.
    listing.me = remote_me().await.or_else(|| {
        let name = remote_user();
        (!name.is_empty()).then_some(Me { name, admin: false, logout: None })
    });
    if let Some(m) = &listing.me {
        REMOTE_USER.with(|u| *u.borrow_mut() = m.name.clone());
    }
    Ok(Some(listing))
}

/// Keep the version a response says a design is at, and where in its log that version stands.
fn remember_etag(name: &str, r: &web_sys::Response) {
    let tag = r.headers().get("etag").ok().flatten().map(|t| t.trim().trim_start_matches("W/").trim_matches('"').to_string());
    ETAGS.with(|e| {
        e.borrow_mut().insert(name.to_string(), tag);
    });
    let seq = r.headers().get("x-ops-seq").ok().flatten().and_then(|s| s.parse::<u64>().ok());
    let commit = r.headers().get("x-ops-commit").ok().flatten().unwrap_or_default();
    if let Some(seq) = seq {
        BASES.with(|b| {
            b.borrow_mut().insert(name.to_string(), (seq, commit));
        });
    }
}

/// A design's versions, newest first.
async fn remote_versions(name: &str) -> Result<Vec<Version>, String> {
    let r = remote_request("GET", &format!("{}?history", remote_url(name)), None, &[]).await?;
    if !is_listing(&r) {
        return Err(if r.ok() { "this server keeps no history".to_string() } else { format!("HTTP {}", r.status()) });
    }
    Ok(json_rows(r)
        .await?
        .iter()
        .filter_map(|row| {
            Some(Version {
                hash: row.get("hash")?.clone(),
                author: row.get("author").cloned().unwrap_or_default(),
                date: date_of(row, "date"),
                message: row.get("message").cloned().unwrap_or_default(),
                size: row.get("size").and_then(|s| s.parse().ok()).unwrap_or(0),
            })
        })
        .collect())
}

async fn remote_read(name: &str) -> Result<Vec<u8>, String> {
    let r = remote_request("GET", &remote_url(name), None, &[]).await?;
    if !r.ok() {
        return Err(format!("HTTP {}", r.status()));
    }
    remember_etag(name, &r);
    let buf = JsFuture::from(r.array_buffer().map_err(err)?).await.map_err(err)?;
    Ok(Uint8Array::new(&buf).to_vec())
}

/// Save to the server, replacing the version this browser holds and no other: a design nobody
/// here has seen is written only where there is none yet.
async fn remote_write(name: &str, bytes: &[u8]) -> Result<(), String> {
    let known = ETAGS.with(|e| e.borrow().get(name).cloned());
    let precondition = match &known {
        Some(Some(tag)) => Some(("If-Match", format!("\"{tag}\""))),
        Some(None) => None,
        None => Some(("If-None-Match", "*".to_string())),
    };
    let mut extra: Vec<(&str, String)> = precondition.into_iter().collect();
    if let Some((n, fp)) = FINGERPRINT.with(|f| f.borrow().clone())
        && n == name
    {
        extra.push(("X-Fingerprint", fp));
    }
    let headers: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let r = remote_request("PUT", &remote_url(name), Some(bytes), &headers).await?;
    if r.ok() {
        remember_etag(name, &r);
        return Ok(());
    }
    if r.status() == 403 {
        return Err("you may only view this design here: ask its owner to share it for editing, or save it under another name".into());
    }
    if r.status() == 412 {
        // Someone else saved since the version here: the next save overwrites.
        ETAGS.with(|e| {
            e.borrow_mut().insert(name.to_string(), None);
        });
        let who = r.headers().get("x-author").ok().flatten().unwrap_or_else(|| "someone else".into());
        let when = r
            .headers()
            .get("x-author-date")
            .ok()
            .flatten()
            .map(|d| js_sys::Date::parse(&d))
            .filter(|t| !t.is_nan())
            .map(|t| format!(" {}", ago(t)))
            .unwrap_or_default();
        return Err(format!("{who} saved this design on the server{when}. Save again to overwrite it, or save it under another name"));
    }
    Err(format!("the server's design folder refused the write (HTTP {})", r.status()))
}

async fn remote_remove(name: &str) -> Result<(), String> {
    ETAGS.with(|e| e.borrow_mut().remove(name));
    let r = remote_request("DELETE", &remote_url(name), None, &[]).await?;
    // Gone already is gone.
    if r.ok() || r.status() == 404 { Ok(()) } else { Err(format!("the server's design folder refused the delete (HTTP {})", r.status())) }
}

/// Fetch the current folder's recent changes again (after a save, delete or restore).
fn refresh_changes(files: &Rc<Files>) {
    if !remote_only() {
        return;
    }
    let files = files.clone();
    spawn_local(async move {
        let changes = remote_log(&remote_dir()).await;
        *files.changes.borrow_mut() = changes;
        files.wake();
    });
}

/// Open the history window on a server design: its versions arrive when the server answers.
pub fn history(files: &Rc<Files>, name: &str) {
    *files.history.borrow_mut() = Some((name.to_string(), Vec::new()));
    let (files, name) = (files.clone(), name.to_string());
    spawn_local(async move {
        match remote_versions(&name).await {
            Ok(versions) => {
                if let Some((n, v)) = files.history.borrow_mut().as_mut()
                    && *n == name
                {
                    *v = versions;
                }
            }
            Err(e) => {
                *files.history.borrow_mut() = None;
                files.say(format!("{name}: {e}"), true);
            }
        }
        files.wake();
    });
}

/// Make a version the current one again on the server (a new version: nothing is lost), and
/// bring it in here. A copy of the design open in the app is not changed: reopen it.
pub fn restore(files: &Rc<Files>, name: &str, hash: &str) {
    let (files, name, hash) = (files.clone(), name.to_string(), hash.to_string());
    spawn_local(async move {
        let r = async {
            let r = remote_request("POST", &format!("{}?restore={hash}", remote_url(&name)), None, &[]).await?;
            if !r.ok() {
                return Err(format!("the server refused the restore (HTTP {})", r.status()));
            }
            let bytes = remote_read(&name).await?;
            let size = bytes.len() as u64;
            vfs::insert(&format!("{OPFS}{name}"), bytes);
            if let Some(e) = files.entries.borrow_mut().iter_mut().find(|e| e.name == name) {
                e.size = size;
                e.modified = js_sys::Date::now();
            }
            Ok::<(), String>(())
        };
        match r.await {
            Ok(()) => {
                files.say(format!("{name}: that version is the current one again (reopen the design to see it)"), false);
                history(&files, &name);
                refresh_changes(&files);
            }
            Err(e) => files.say(format!("{name}: {e}"), true),
        }
    });
}

/// A branch of a server design: a copy under `new_name` (in the current folder) whose log
/// remembers where it came from, to work on apart and merge back later (sync.rs).
pub fn branch(files: &Rc<Files>, name: &str, new_name: &str) {
    let new_name = new_name.trim().to_string();
    if new_name.is_empty() || new_name.contains(['/', '\\']) {
        files.say("give the branch a name without slashes", true);
        return;
    }
    let (files, name) = (files.clone(), name.to_string());
    spawn_local(async move {
        let folder = remote_folder();
        let to = if folder.is_empty() { new_name.clone() } else { format!("{folder}/{new_name}") };
        let r = async {
            let url = format!("{}?branch={}", remote_url(&name), String::from(js_sys::encode_uri_component(&to)));
            let r = remote_request("POST", &url, None, &[]).await?;
            if !r.ok() {
                let why = JsFuture::from(r.text().map_err(err)?).await.ok().and_then(|t| t.as_string()).unwrap_or_default();
                return Err(format!("the server refused the branch (HTTP {}): {}", r.status(), why.trim()));
            }
            let bytes = remote_read(&new_name).await?;
            let size = bytes.len() as u64;
            vfs::insert(&format!("{OPFS}{new_name}"), bytes);
            let mut e = files.entries.borrow_mut();
            e.retain(|x| x.name != new_name);
            e.push(Entry { name: new_name.clone(), size, modified: js_sys::Date::now() });
            e.sort_by_key(|a| a.name.to_lowercase());
            Ok::<(), String>(())
        };
        match r.await {
            Ok(()) => {
                files.say(format!("{new_name}: a branch of {name}; open it to work apart, merge it back from its history"), false);
                refresh_changes(&files);
            }
            Err(e) => files.say(format!("{name}: {e}"), true),
        }
    });
}

/// Bring a version in as a separate design beside the current one, named after its date.
pub fn copy_version(files: &Rc<Files>, name: &str, hash: &str, date: f64) {
    let (files, name, hash) = (files.clone(), name.to_string(), hash.to_string());
    spawn_local(async move {
        let r = async {
            let r = remote_request("GET", &format!("{}?at={hash}", remote_url(&name)), None, &[]).await?;
            if !r.ok() {
                return Err(format!("the server has no such version (HTTP {})", r.status()));
            }
            let buf = JsFuture::from(r.array_buffer().map_err(err)?).await.map_err(err)?;
            let bytes = Uint8Array::new(&buf).to_vec();
            let (stem, ext) = name.rsplit_once('.').map(|(s, e)| (s.to_string(), format!(".{e}"))).unwrap_or((name.clone(), String::new()));
            let stamp: String = String::from(js_sys::Date::new(&JsValue::from_f64(date)).to_iso_string())
                .chars()
                .take(16)
                .map(|c| {
                    if c == 'T' {
                        ' '
                    } else if c == ':' {
                        '.'
                    } else {
                        c
                    }
                })
                .collect();
            let path = free_name(&format!("{stem} ({stamp}){ext}"));
            vfs::write(&path, &bytes).map_err(|e| e.to_string())?;
            Ok::<String, String>(path)
        };
        match r.await {
            Ok(path) => {
                flush(&files);
                files.say(format!("{}: a copy of that version", path.trim_start_matches(OPFS)), false);
            }
            Err(e) => files.say(format!("{name}: {e}"), true),
        }
    });
}

fn err(e: JsValue) -> String {
    e.as_string().or_else(|| Reflect::get(&e, &"message".into()).ok().and_then(|m| m.as_string())).unwrap_or_else(|| format!("{e:?}"))
}

/// The browser's storage manager: missing on insecure pages (http on a network address) and in
/// old browsers, where calling into it would throw.
fn storage_manager() -> Option<web_sys::StorageManager> {
    let nav = web_sys::window()?.navigator();
    let sm = Reflect::get(&nav, &"storage".into()).ok().filter(|v| v.is_object())?;
    Some(sm.unchecked_into())
}

/// Does the storage manager have `method`?
fn has(sm: &web_sys::StorageManager, method: &str) -> bool {
    Reflect::get(sm, &method.into()).is_ok_and(|f| f.is_function())
}

async fn root() -> Result<FileSystemDirectoryHandle, String> {
    let sm = storage_manager().filter(|sm| has(sm, "getDirectory")).ok_or("this page has no private file storage (it needs https or localhost)")?;
    let dir = JsFuture::from(sm.get_directory()).await.map_err(err)?;
    dir.dyn_into::<FileSystemDirectoryHandle>().map_err(|_| "this browser has no private file storage (OPFS)".to_string())
}

async fn read_handle(h: &FileSystemFileHandle) -> Result<Vec<u8>, String> {
    let file: web_sys::File = JsFuture::from(h.get_file()).await.map_err(err)?.dyn_into().map_err(|_| "not a file".to_string())?;
    blob_bytes(&file).await
}

async fn blob_bytes(b: &web_sys::Blob) -> Result<Vec<u8>, String> {
    let buf = JsFuture::from(b.array_buffer()).await.map_err(err)?;
    Ok(Uint8Array::new(&buf).to_vec())
}

async fn file_handle(dir: &FileSystemDirectoryHandle, name: &str, create: bool) -> Result<FileSystemFileHandle, String> {
    let opts = FileSystemGetFileOptions::new();
    opts.set_create(create);
    JsFuture::from(dir.get_file_handle_with_options(name, &opts)).await.map_err(err)?.dyn_into().map_err(|_| "not a file".to_string())
}

async fn write_file(name: &str, bytes: &[u8]) -> Result<(), String> {
    if remote_only() && !stays_local(name) {
        return remote_write(name, bytes).await;
    }
    if local_only() {
        return local()?
            .set_item(&format!("{LOCAL_PREFIX}{name}"), &encode(bytes))
            .map_err(|e| format!("local storage is full or blocked: {}", err(e)));
    }
    let dir = root().await?;
    let h = file_handle(&dir, name, true).await?;
    let w: FileSystemWritableFileStream =
        JsFuture::from(h.create_writable()).await.map_err(err)?.dyn_into().map_err(|_| "no writable stream".to_string())?;
    JsFuture::from(w.write_with_u8_array(bytes).map_err(err)?).await.map_err(err)?;
    JsFuture::from(w.close()).await.map_err(err)?;
    Ok(())
}

async fn remove_file(name: &str) -> Result<(), String> {
    if remote_only() && !stays_local(name) {
        return remote_remove(name).await;
    }
    if local_only() {
        let st = local()?;
        st.remove_item(&format!("{LEGACY_LOCAL_PREFIX}{name}")).map_err(err)?;
        return st.remove_item(&format!("{LOCAL_PREFIX}{name}")).map_err(err);
    }
    let dir = root().await?;
    JsFuture::from(dir.remove_entry(name)).await.map_err(err)?;
    Ok(())
}

/// Every stored file in local storage.
fn read_local() -> Result<Vec<(String, Vec<u8>, f64)>, String> {
    let st = local()?;
    let mut out = Vec::new();
    for i in 0..st.length().unwrap_or(0).min(10_000) {
        let Some(k) = st.key(i).ok().flatten() else { continue };
        let Some(name) = k.strip_prefix(LOCAL_PREFIX).or_else(|| k.strip_prefix(LEGACY_LOCAL_PREFIX)) else { continue };
        if let Some(v) = st.get_item(&k).ok().flatten() {
            out.push((name.to_string(), decode(&v), 0.0));
        }
    }
    Ok(out)
}

/// A stored file: name, bytes, modified.
type Stored = (String, Vec<u8>, f64);

/// What a load brings in: the stored files, and with server storage the folder's subfolders and
/// recent changes.
#[derive(Default)]
struct Loaded {
    files: Vec<Stored>,
    folders: Vec<String>,
    changes: Vec<Change>,
    shared: Vec<Shared>,
    me: Option<Me>,
    access: String,
}

/// Every stored file, and the server folder's subfolders. With server storage, the server's files
/// and the browser's autosave; a server that is there but fails keeps designs out of the browser
/// (saves report the failure) rather than hiding them in it until the next reload.
async fn read_all() -> Result<Loaded, String> {
    let list = match remote_list().await {
        Ok(None) => return Ok(Loaded { files: read_browser().await?, ..Default::default() }),
        r => r,
    };
    REMOTE.with(|r| r.set(true));
    let mut loaded = read_remote(list?.unwrap_or_default()).await;
    match read_browser().await {
        Ok(b) => loaded.files.extend(b.into_iter().filter(|(n, _, _)| stays_local(n))),
        Err(e) => log::warn!("browser storage: {e}"),
    }
    Ok(loaded)
}

/// The listed server files, read; the subfolders and changes as they are.
async fn read_remote(listing: Listing) -> Loaded {
    let mut files = Vec::new();
    for (name, modified) in listing.files {
        match remote_read(&name).await {
            Ok(b) => files.push((name, b, modified)),
            Err(e) => log::warn!("{name}: {e}"),
        }
    }
    Loaded { files, folders: listing.folders, changes: listing.changes, shared: listing.shared, me: listing.me, access: listing.access }
}

/// Every file in the browser's storage. Without OPFS, local storage takes over from here on.
async fn read_browser() -> Result<Vec<(String, Vec<u8>, f64)>, String> {
    let dir = match root().await {
        Ok(d) => d,
        Err(e) => {
            log::warn!("no OPFS ({e}): designs go to local storage");
            LOCAL.with(|l| l.set(true));
            return read_local();
        }
    };
    let iter: JsValue = dir.values().into();
    let next: Function = Reflect::get(&iter, &"next".into()).map_err(err)?.dyn_into().map_err(|_| "no iterator".to_string())?;
    let mut out = Vec::new();
    for _ in 0..10_000 {
        let step = JsFuture::from(js_sys::Promise::resolve(&next.call0(&iter).map_err(err)?)).await.map_err(err)?;
        if Reflect::get(&step, &"done".into()).ok().and_then(|d| d.as_bool()).unwrap_or(true) {
            break;
        }
        let Ok(h) = Reflect::get(&step, &"value".into()).map_err(err)?.dyn_into::<FileSystemFileHandle>() else { continue };
        let file =
            match JsFuture::from(h.get_file()).await.map_err(err).and_then(|f| f.dyn_into::<web_sys::File>().map_err(|_| "not a file".to_string())) {
                Ok(f) => f,
                Err(e) => {
                    log::warn!("{}: {e}", h.name());
                    continue;
                }
            };
        match blob_bytes(&file).await {
            Ok(b) => out.push((h.name(), b, file.last_modified())),
            Err(e) => log::warn!("{}: {e}", h.name()),
        }
    }
    Ok(out)
}

/// Ask the browser to keep the storage (not evict it under pressure), then load every stored
/// file into the engine's files.
pub fn start(files: &Rc<Files>) {
    if let Some(sm) = storage_manager().filter(|sm| has(sm, "persist"))
        && let Ok(p) = sm.persist()
    {
        spawn_local(async move {
            let _ = JsFuture::from(p).await;
        });
    }
    REMOTE_FOLDER.with(|f| *f.borrow_mut() = stored_folder());
    let files = files.clone();
    spawn_local(async move {
        let loaded = read_all().await;
        if loaded.is_ok() && local_only() && !remote_only() {
            files.say(
                "This page can't use the browser's file storage (it needs https or localhost): designs are kept in local storage, a few MB at most.",
                false,
            );
        }
        install(&files, loaded);
        if remote_only() {
            fetch_libraries(&files);
        }
    });
}

/// The loaded files go into the engine's files and the panel's entries; the app may open them.
fn install(files: &Rc<Files>, loaded: Result<Loaded, String>) {
    match loaded {
        Ok(Loaded { files: all, folders, changes, shared, me, access }) => {
            // New features are the signed-in user's (the timeline says whose each is).
            solidcraft_engine::sync::set_author(me.as_ref().map(|m| m.name.clone()));
            *files.me.borrow_mut() = me;
            *files.folder_access.borrow_mut() = access;
            files.shared_at.set(js_sys::Date::now());
            let mut entries = Vec::new();
            let has_recovery = all.iter().any(|(n, _, _)| n == RECOVERY);
            for (name, bytes, modified) in all {
                if name == LEGACY_RECOVERY {
                    if !has_recovery {
                        vfs::insert(&format!("{OPFS}{RECOVERY}"), bytes);
                    }
                    continue;
                }
                if name == RECOVERY_META {
                    let meta: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
                    let n = meta["name"].as_str().unwrap_or("Untitled").to_string();
                    *files.recovery.borrow_mut() = Some((n, meta["path"].as_str().map(str::to_string)));
                    continue;
                }
                if name != RECOVERY {
                    entries.push(Entry { name: name.clone(), size: bytes.len() as u64, modified });
                }
                vfs::insert(&format!("{OPFS}{name}"), bytes);
            }
            entries.sort_by_key(|a| a.name.to_lowercase());
            *files.entries.borrow_mut() = entries;
            *files.folders.borrow_mut() = folders;
            *files.changes.borrow_mut() = changes;
            fetch_shared_files(files, &shared);
            *files.shared.borrow_mut() = shared;
            // An autosave without its design file is nothing to recover.
            if !vfs::exists(&format!("{OPFS}{RECOVERY}")) {
                *files.recovery.borrow_mut() = None;
            }
        }
        Err(e) => files.say(format!("storage: {e}"), true),
    }
    files.ready.set(true);
    files.wake();
}

/// File designs in `folder` on the server: "" for the design folder itself, else a relative path
/// like "projects/bracket" (made on the first save). The panel's entries are reloaded from there,
/// and the browser remembers the folder.
pub fn set_remote_folder(files: &Rc<Files>, folder: &str) -> Result<(), String> {
    let folder = clean_folder(folder)?;
    if folder == remote_folder() {
        return Ok(());
    }
    if let Ok(st) = local() {
        let _ = if folder.is_empty() { st.remove_item(REMOTE_FOLDER_KEY) } else { st.set_item(REMOTE_FOLDER_KEY, &folder) };
    }
    REMOTE_FOLDER.with(|f| *f.borrow_mut() = folder);
    // The old folder's designs leave the engine's files (the autosave stays).
    for e in files.entries.borrow_mut().drain(..) {
        let _ = vfs::remove(&format!("{OPFS}{}", e.name));
    }
    files.folders.borrow_mut().clear();
    files.changes.borrow_mut().clear();
    *files.history.borrow_mut() = None;
    // Names mean other files in another folder.
    ETAGS.with(|e| e.borrow_mut().clear());
    BASES.with(|b| b.borrow_mut().clear());
    files.ready.set(false);
    let files = files.clone();
    spawn_local(async move {
        let loaded = match remote_list().await {
            Ok(list) => Ok(read_remote(list.unwrap_or_default()).await),
            Err(e) => Err(e),
        };
        install(&files, loaded);
    });
    Ok(())
}

/// Store what commands wrote: OPFS paths into OPFS, download paths as downloads.
pub fn flush(files: &Rc<Files>) {
    for (path, bytes) in vfs::take_writes() {
        if let Some(name) = path.strip_prefix(OPFS) {
            let name = name.to_string();
            {
                let mut e = files.entries.borrow_mut();
                e.retain(|x| x.name != name);
                if name != RECOVERY && name != RECOVERY_META {
                    e.push(Entry { name: name.clone(), size: bytes.len() as u64, modified: js_sys::Date::now() });
                    e.sort_by_key(|a| a.name.to_lowercase());
                }
            }
            let files = files.clone();
            spawn_local(async move {
                match write_file(&name, &bytes).await {
                    Ok(()) if !stays_local(&name) => refresh_changes(&files),
                    Ok(()) => {}
                    Err(e) => files.say(format!("{name}: {e}"), true),
                }
            });
        } else if let Some(name) = path.strip_prefix(DOWNLOADS)
            && let Err(e) = download(name, &bytes)
        {
            files.say(format!("{name}: {e}"), true);
        }
    }
}

/// Hand `bytes` to the user as a download named `name`.
pub fn download(name: &str, bytes: &[u8]) -> Result<(), String> {
    let doc = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let parts = Array::new();
    parts.push(&Uint8Array::from(bytes));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("application/octet-stream");
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts).map_err(err)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(err)?;
    let a: web_sys::HtmlAnchorElement = doc.create_element("a").map_err(err)?.dyn_into().map_err(|_| "no anchor".to_string())?;
    a.set_href(&url);
    a.set_download(name);
    a.click();
    // Let the browser start the download before the URL goes.
    let revoke = Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&url);
    });
    if let Some(w) = web_sys::window() {
        let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 10_000);
    }
    Ok(())
}

/// Delete a stored design.
pub fn delete(files: &Rc<Files>, name: &str) {
    let _ = vfs::remove(&format!("{OPFS}{name}"));
    files.entries.borrow_mut().retain(|e| e.name != name);
    let (files, name) = (files.clone(), name.to_string());
    spawn_local(async move {
        match remove_file(&name).await {
            Ok(()) => refresh_changes(&files),
            Err(e) => files.say(format!("{name}: {e}"), true),
        }
    });
}

/// Rename a stored design (a copy under the new name, then the old one goes: not every browser
/// can move OPFS files).
pub fn rename(files: &Rc<Files>, from: &str, to: &str) -> Result<(), String> {
    let to = to.trim();
    if to.is_empty() || to.contains('/') || to.contains('\\') || to.len() > 200 {
        return Err("give a file name without slashes".into());
    }
    if files.entries.borrow().iter().any(|e| e.name == to) {
        return Err(format!("there is already a file named {to}"));
    }
    let bytes = vfs::read(&format!("{OPFS}{from}"), solidcraft_engine::io::MAX_DESIGN_BYTES).map_err(|e| e.to_string())?;
    vfs::write(&format!("{OPFS}{to}"), &bytes).map_err(|e| e.to_string())?;
    delete(files, from);
    flush(files);
    Ok(())
}

/// Keep (or drop) the autosave of the open design.
pub fn autosave(files: &Rc<Files>, doc: Option<(&solidcraft_engine::doc::Document, Option<&str>)>) {
    let files = files.clone();
    match doc {
        Some((d, path)) => {
            let bytes = match solidcraft_engine::io::write_design_container(d) {
                Ok(bytes) => bytes,
                Err(e) => {
                    files.say(format!("autosave: {e}"), true);
                    return;
                }
            };
            let meta = serde_json::json!({"name": d.name, "path": path, "saved_at": js_sys::Date::now()}).to_string();
            spawn_local(async move {
                let r = async {
                    write_file(RECOVERY, &bytes).await?;
                    write_file(RECOVERY_META, meta.as_bytes()).await
                };
                if let Err(e) = r.await {
                    files.say(format!("autosave: {e}"), true);
                }
            });
        }
        None => spawn_local(async move {
            let _ = remove_file(RECOVERY).await;
            let _ = remove_file(LEGACY_RECOVERY).await;
            let _ = remove_file(RECOVERY_META).await;
        }),
    }
}

/// A stored path for `name` that no stored file has: `name`, else `name (2)`, `name (3)`…
pub fn free_name(name: &str) -> String {
    let (stem, ext) = name.rsplit_once('.').map(|(s, e)| (s.to_string(), format!(".{e}"))).unwrap_or((name.to_string(), String::new()));
    for i in 1..1000 {
        let n = if i == 1 { name.to_string() } else { format!("{stem} ({i}){ext}") };
        let p = format!("{OPFS}{n}");
        if !vfs::exists(&p) {
            return p;
        }
    }
    format!("{OPFS}{stem} ({}){ext}", js_sys::Date::now() as u64)
}

/// The autosaved design's path (in the engine's files) for opening it.
pub fn recovery_path() -> String {
    format!("{OPFS}{RECOVERY}")
}

/// Pick files on the computer: the File System Access picker where there is one, else an
/// `<input type=file>`. Read files arrive in `files.incoming`.
pub fn pick_from_disk(files: &Rc<Files>, intent: Intent) {
    let Some(window) = web_sys::window() else { return };
    let picker = Reflect::get(&window, &"showOpenFilePicker".into()).ok().and_then(|f| f.dyn_into::<Function>().ok());
    match picker {
        Some(f) => {
            let opts = Object::new();
            let _ = Reflect::set(&opts, &"multiple".into(), &JsValue::FALSE);
            let promise = f.call1(&window, &opts);
            let files = files.clone();
            spawn_local(async move {
                let r = async {
                    let p: js_sys::Promise = promise.map_err(err)?.dyn_into().map_err(|_| "no promise".to_string())?;
                    let handles: Array = JsFuture::from(p).await.map_err(err)?.dyn_into().map_err(|_| "no files".to_string())?;
                    for h in handles.iter() {
                        let h: FileSystemFileHandle = h.dyn_into().map_err(|_| "not a file".to_string())?;
                        let bytes = read_handle(&h).await?;
                        files.arrive(h.name(), bytes, intent);
                    }
                    Ok::<(), String>(())
                };
                match r.await {
                    Ok(()) => {}
                    // Closing the picker is no error.
                    Err(e) if e.contains("abort") || e.contains("Abort") => {}
                    Err(e) => files.say(format!("open: {e}"), true),
                }
            });
        }
        None => input_picker(files, intent),
    }
}

fn input_picker(files: &Rc<Files>, intent: Intent) {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return };
    let Ok(input) = doc.create_element("input").map(|e| e.unchecked_into::<web_sys::HtmlInputElement>()) else { return };
    input.set_type("file");
    input.set_accept(".solidcraft,.solvecraft,.json,.step,.stp,.3mf,.stl,.obj,.glb,.gltf");
    // Attached (hidden) while it is open: some browsers ignore a click on a detached input.
    let _ = input.set_attribute("style", "display:none");
    if let Some(body) = doc.body() {
        let _ = body.append_child(&input);
    }
    let (files, el) = (files.clone(), input.clone());
    let on_change = Closure::once_into_js(move || {
        el.remove();
        let Some(list) = el.files() else { return };
        for i in 0..list.length() {
            let Some(f) = list.get(i) else { continue };
            let files = files.clone();
            spawn_local(async move {
                match blob_bytes(&f).await {
                    Ok(b) => files.arrive(f.name(), b, intent),
                    Err(e) => files.say(format!("{}: {e}", f.name()), true),
                }
            });
        }
    });
    input.set_onchange(Some(on_change.unchecked_ref()));
    input.click();
}

/// Read files dropped on the page.
pub fn take_drops(files: &Rc<Files>, raw: &mut egui::RawInput) {
    for f in std::mem::take(&mut raw.dropped_files) {
        let files = files.clone();
        spawn_local(async move {
            let name = f.path().file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "dropped".into());
            match f.bytes_async().await {
                Ok(b) => files.arrive(name, b, Intent::Insert),
                Err(e) => files.say(format!("{name}: {e}"), true),
            }
        });
    }
}
