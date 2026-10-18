//! The HTTP side: routes under `/files/` onto the store (see the crate doc for the contract).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::Settings;
use crate::access::{self, Acl, Level};
use crate::directory::Directory;
use crate::ops::Refused;
use crate::store::{Change, Entry, Meta, Store, Version, Written, clean_path};
use crate::users;

/// The largest design accepted (nginx has the same limit).
const MAX_BODY: usize = 200 << 20;
/// Entries of a folder's change log.
const LOG_ENTRIES: usize = 30;
/// nginx passes the signed-in name here; without nginx (or auth) everyone is this.
const USER_HEADER: &str = "X-Remote-User";
const ANONYMOUS: &str = "anonymous";

/// What every request handler sees.
struct Service {
    store: Store,
    users_file: PathBuf,
    allow_anonymous: bool,
    admin_group: String,
    /// Everyone who ever signed in (kept in `seen_file`).
    seen: std::sync::Mutex<std::collections::BTreeSet<String>>,
    seen_file: PathBuf,
    /// Who may see and change what.
    acl: Acl,
    /// The identity provider's users, when it is configured.
    directory: Option<Directory>,
    /// Where Sign out goes when the proxy's `X-Remote-Logout` doesn't say.
    logout_url: Option<String>,
}

impl Service {
    /// Remember a name that signed in; `true` when it is new.
    fn saw(&self, user: &str) -> bool {
        let Ok(mut seen) = self.seen.lock() else { return false };
        if !seen.insert(user.to_string()) {
            return false;
        }
        let text: String = seen.iter().map(|u| format!("{u}\n")).collect();
        if let Err(e) = std::fs::write(&self.seen_file, text) {
            log::warn!("{}: {e}", self.seen_file.display());
        }
        true
    }

    /// Is this user an admin: by the users file, by a group the proxy passed, or by the
    /// directory (a superuser, or a member of the admin group)?
    fn is_admin(&self, user: &str, groups: Option<&str>) -> bool {
        users::is_admin(&self.users_file, user)
            || groups.is_some_and(|g| g.split(',').any(|g| g.trim() == self.admin_group))
            || self.directory.as_ref().is_some_and(|d| d.is_admin(user))
    }
}

pub fn serve(settings: &Settings) -> Result<(), String> {
    let seen_file = settings.seen_file();
    let seen = std::fs::read_to_string(&seen_file)
        .map(|t| t.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    let service = Arc::new(Service {
        store: Store::open(&settings.designs, &settings.history)?,
        users_file: settings.users_file(),
        allow_anonymous: settings.allow_anonymous,
        admin_group: settings.admin_group.clone(),
        seen: std::sync::Mutex::new(seen),
        seen_file,
        acl: Acl::open(&settings.history.join("access.json"), settings.default_open),
        directory: Directory::new(&settings.authentik_url, &settings.authentik_token, &settings.admin_group),
        logout_url: Some(settings.logout_url.trim().to_string()).filter(|u| !u.is_empty()),
    });
    if let Some(d) = &service.directory {
        log::info!("users also come from {} ({} known now)", d.source(), d.users().len());
    }
    let server = Server::http(&settings.listen).map_err(|e| format!("listen on {}: {e}", settings.listen))?;
    log::info!("serving {} on http://{}/files/ (history in {})", settings.designs.display(), settings.listen, settings.history.display());
    for request in server.incoming_requests() {
        let service = service.clone();
        std::thread::spawn(move || handle(&service, request));
    }
    Ok(())
}

fn handle(service: &Service, mut request: Request) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let named = header(&request, USER_HEADER).filter(|u| !u.is_empty() && users::check_name(u).is_ok());
    if named.is_none() && !service.allow_anonymous && url != "/healthz" {
        log::info!("- {method} {url} -> 401 (no user name from the proxy)");
        let _ = request.respond(Response::from_string("sign in first (the proxy passed no user name)\n").with_status_code(401));
        return;
    }
    // A proxy that knows people by email (Cloudflare Access) names the directory's user.
    let user = named.map(|n| service.directory.as_ref().map_or_else(|| n.clone(), |d| d.resolve(&n))).unwrap_or_else(|| ANONYMOUS.to_string());
    if user != ANONYMOUS && service.saw(&user) {
        log::info!("{user}: first sign-in");
    }
    let admin = service.is_admin(&user, header(&request, "X-Remote-Groups").as_deref());
    // The proxy's sign-out page (`?me`): on this site, or https; never anything else.
    let logout =
        header(&request, "X-Remote-Logout").filter(|u| u.starts_with('/') || u.starts_with("https://")).or_else(|| service.logout_url.clone());
    let if_match = header(&request, "If-Match").map(|v| v.trim().trim_start_matches("W/").trim_matches('"').to_string());
    let only_new = header(&request, "If-None-Match").is_some_and(|v| v.trim() == "*");
    let fingerprint = header(&request, "X-Fingerprint").filter(|f| !f.is_empty() && f.len() <= 64);
    let body = if matches!(method, Method::Put | Method::Post | Method::Delete) { read_body(&mut request) } else { Ok(Vec::new()) };
    let reply = match body {
        Ok(body) => match users_route(service, admin, &method, &url, &user, &body, logout.as_deref()) {
            Some(reply) => reply,
            None => route(service, admin, &method, &url, &user, if_match.as_deref(), only_new, fingerprint.as_deref(), body),
        },
        Err(e) => Reply::text(413, &e),
    };
    log::info!("{user} {method} {url} -> {}", reply.status);
    let mut response = Response::from_data(reply.body).with_status_code(reply.status);
    for (k, v) in reply.headers {
        if let Ok(h) = Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            response = response.with_header(h);
        }
    }
    if let Ok(h) = Header::from_bytes("Cache-Control", "no-store") {
        response = response.with_header(h);
    }
    if let Err(e) = request.respond(response) {
        log::warn!("{method} {url}: reply lost: {e}");
    }
}

fn header(request: &Request, name: &'static str) -> Option<String> {
    request.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_string())
}

fn read_body(request: &mut Request) -> Result<Vec<u8>, String> {
    let declared = request.body_length().unwrap_or(0);
    if declared > MAX_BODY {
        return Err(format!("a design can be {} MB at most", MAX_BODY >> 20));
    }
    let mut body = Vec::with_capacity(declared);
    request.as_reader().take(MAX_BODY as u64 + 1).read_to_end(&mut body).map_err(|e| format!("reading the body: {e}"))?;
    if body.len() > MAX_BODY {
        return Err(format!("a design can be {} MB at most", MAX_BODY >> 20));
    }
    Ok(body)
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn new(status: u16) -> Self {
        Self { status, headers: Vec::new(), body: Vec::new() }
    }

    fn text(status: u16, text: &str) -> Self {
        Self { status, headers: vec![("Content-Type".into(), "text/plain; charset=utf-8".into())], body: format!("{text}\n").into_bytes() }
    }

    fn json(status: u16, value: &Value) -> Self {
        Self { status, headers: vec![("Content-Type".into(), "application/json".into())], body: value.to_string().into_bytes() }
    }

    fn bytes(bytes: Vec<u8>) -> Self {
        Self { status: 200, headers: vec![("Content-Type".into(), "application/octet-stream".into())], body: bytes }
    }

    fn with(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    fn with_meta(self, meta: &Meta) -> Self {
        let mut r = self.with("ETag", &format!("\"{}\"", meta.etag));
        if !meta.author.is_empty() {
            r = r.with("X-Author", &meta.author).with("X-Author-Date", &meta.date);
        }
        if let Some((seq, commit)) = &meta.base {
            r = r.with("X-Ops-Seq", &seq.to_string()).with("X-Ops-Commit", commit);
        }
        r
    }
}

/// `path?query` → (decoded path, query pairs).
fn split_url(url: &str) -> (String, Vec<(String, String)>) {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let decode = |s: &str| percent_decode_str(s).decode_utf8_lossy().into_owned();
    let query = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect();
    (decode(path), query)
}

#[allow(clippy::too_many_arguments)]
fn route(
    service: &Service,
    admin: bool,
    method: &Method,
    url: &str,
    user: &str,
    if_match: Option<&str>,
    only_new: bool,
    fingerprint: Option<&str>,
    body: Vec<u8>,
) -> Reply {
    let (path, query) = split_url(url);
    let get = |k: &str| query.iter().find(|(q, _)| q == k).map(|(_, v)| v.as_str());
    if path == "/healthz" {
        return Reply::text(200, "ok");
    }
    let Some(rest) = path.strip_prefix("/files/") else {
        return Reply::text(404, "not here: the designs are under /files/");
    };
    let folder = rest.ends_with('/') || rest.is_empty();
    let clean = match clean_path(rest) {
        Ok(p) => p,
        Err(e) => return Reply::text(400, &e),
    };
    let store = &service.store;
    store.sweep(false);
    // Sharing: what may this user do here? (Branching only reads the source; its target folder
    // is checked below.)
    let acl = &service.acl;
    let key = access::key(&clean, folder);
    let level = acl.level(user, admin, &key);
    if get("access").is_some() {
        return access_route(service, admin, method, &key, user, &body, level);
    }
    let changes = matches!(method, Method::Put | Method::Delete | Method::Post) && get("branch").is_none();
    // Reading a folder is never refused: its listing and log show only what this user may see
    // (a design shared with them from a folder that isn't), and `?shared` is their own.
    let reads_folder = *method == Method::Get && folder;
    if (level == Level::None && !reads_folder) || (changes && level != Level::Edit) {
        return Reply::text(403, if level == Level::None { "not shared with you" } else { "shared with you to view, not to change" });
    }
    let may_view = |path: &str, dir: bool| acl.level(user, admin, &access::key(path, dir)) > Level::None;
    let result = match (method, folder) {
        (Method::Get, true) if get("shared").is_some() => Ok(Reply::json(
            200,
            &Value::Array(
                acl.shared_with(user)
                    .into_iter()
                    .map(|(k, owner, edit)| json!({"path": k.trim_end_matches('/'), "folder": k.ends_with('/'), "owner": owner, "edit": edit}))
                    .collect(),
            ),
        )),
        (Method::Get, true) if get("log").is_some() => store.log(&clean, LOG_ENTRIES).map(|mut c| {
            c.retain(|c| may_view(&c.path, false));
            Reply::json(200, &changes_json(&c)).with(USER_HEADER, user)
        }),
        (Method::Get, true) => store.list(&clean).map(|l| match l {
            Some(mut entries) => {
                entries.retain(|e| may_view(&if clean.is_empty() { e.name.clone() } else { format!("{clean}/{}", e.name) }, e.dir));
                Reply::json(200, &listing_json(&entries)).with(USER_HEADER, user).with("X-Folder-Access", level_name(level))
            }
            None => Reply::text(404, "no such folder"),
        }),
        (Method::Get, false) if get("ops").is_some() => {
            let since = get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
            let wait = std::time::Duration::from_secs(get("wait").and_then(|s| s.parse().ok()).unwrap_or(0));
            let client = get("client").filter(|c| !c.is_empty()).map(|c| (c, user));
            Ok(Reply::json(200, &store.ops().since(&clean, since, wait, client)))
        }
        (Method::Post, false) if get("ops").is_some() => match serde_json::from_slice::<Value>(&body) {
            Ok(v) => {
                let client = v.get("client").and_then(Value::as_str).unwrap_or("");
                let ops = v.get("ops").and_then(Value::as_array).cloned().unwrap_or_default();
                Ok(match store.ops().append(&clean, user, client, &ops) {
                    Ok(seq) => Reply::json(200, &json!({"seq": seq})),
                    Err(Refused::Locked { component, user }) => Reply::json(409, &json!({"component": component, "user": user})),
                    Err(Refused::Other(e)) => Reply::text(400, &e),
                })
            }
            Err(e) => Ok(Reply::text(400, &format!("not a command list: {e}"))),
        },
        (Method::Post, false) if get("branch").is_some() => match clean_path(get("branch").unwrap_or("")) {
            Ok(to) if acl.level(user, admin, &access::key(to.rsplit_once('/').map_or("", |(d, _)| d), true)) < Level::Edit => {
                Ok(Reply::text(403, "you may not add designs to that folder"))
            }
            Ok(to) => store.branch(&clean, &to, user).map(|r| match r {
                Some(meta) => {
                    if !acl.default_open {
                        acl.set(&to, access::Entry { owner: user.to_string(), view: Some(Vec::new()), edit: Some(Vec::new()) });
                    }
                    Reply::new(201).with_meta(&meta)
                }
                None => Reply::text(404, "no such design"),
            }),
            Err(e) => Ok(Reply::text(400, &e)),
        },
        (Method::Post, false) if get("lock").is_some() => {
            let component = get("lock").and_then(|c| c.parse().ok()).unwrap_or(0);
            let client = get("client").unwrap_or("");
            Ok(match store.ops().lock(&clean, component, user, client) {
                Ok(locks) => Reply::json(200, &json!({"locks": locks})),
                Err(Refused::Locked { component, user }) => Reply::json(409, &json!({"component": component, "user": user})),
                Err(Refused::Other(e)) => Reply::text(500, &e),
            })
        }
        (Method::Post, false) if get("unlock").is_some() => {
            let component = get("unlock").and_then(|c| c.parse().ok()).unwrap_or(0);
            Ok(Reply::json(200, &json!({"locks": store.ops().unlock(&clean, component, user)})))
        }
        (Method::Get, false) if get("history").is_some() => store.history(&clean).map(|v| Reply::json(200, &versions_json(&v))),
        (Method::Get, false) if get("at").is_some() => store.version(&clean, get("at").unwrap_or("")).map(|b| match b {
            Some(bytes) => Reply::bytes(bytes),
            None => Reply::text(404, "no such version of that design"),
        }),
        (Method::Get, false) => store.read(&clean).map(|r| match r {
            Some((bytes, meta)) => Reply::bytes(bytes).with_meta(&meta),
            None => Reply::text(404, "no such design"),
        }),
        (Method::Put, false) if clean.is_empty() => Ok(Reply::text(400, "a design needs a name")),
        (Method::Put, false) => store.write(&clean, &body, user, if_match, only_new, fingerprint).map(|w| match w {
            Written::Created(meta) => {
                // In the "owner" mode a new design starts as its creator's own.
                if !acl.default_open && acl.get(&key).is_none() {
                    acl.set(&key, access::Entry { owner: user.to_string(), view: Some(Vec::new()), edit: Some(Vec::new()) });
                }
                Reply::new(201).with_meta(&meta)
            }
            Written::Updated(meta) | Written::Unchanged(meta) => Reply::new(204).with_meta(&meta),
            Written::Stale(meta) => Reply::json(412, &json!({"author": meta.author, "date": meta.date})).with_meta(&meta),
        }),
        (Method::Delete, false) => store.delete(&clean, user).map(|gone| {
            if gone {
                acl.remove(&key);
                Reply::new(204)
            } else {
                Reply::text(404, "no such design")
            }
        }),
        (Method::Post, false) if get("restore").is_some() => store.restore(&clean, get("restore").unwrap_or(""), user).map(|r| match r {
            Some(meta) => Reply::new(200).with_meta(&meta),
            None => Reply::text(404, "no such version of that design"),
        }),
        (Method::Put | Method::Delete | Method::Post, true) => Ok(Reply::text(405, "folders are made by saving into them")),
        _ => Ok(Reply::text(405, "method not allowed")),
    };
    match result {
        Ok(reply) => reply,
        Err(e) => {
            log::error!("{method} {url}: {e}");
            Reply::text(500, &e)
        }
    }
}

/// `?access`: the sharing of a design or folder. Anyone who may view it sees it; owners, admins
/// and the creator of a design without an entry of its own set it (`POST`) or let it follow its
/// folder again (`DELETE`).
fn access_route(service: &Service, admin: bool, method: &Method, key: &str, user: &str, body: &[u8], level: Level) -> Reply {
    let acl = &service.acl;
    let own = acl.get(key);
    let creator = || !key.ends_with('/') && service.store.first_author(key).as_deref() == Some(user);
    // A folder nobody owns yet is taken by whoever may change it (the top folder by admins only).
    let claimable = key.ends_with('/') && key != "/" && level == Level::Edit;
    let can_manage = acl.manages(user, admin, key) || (own.is_none() && (admin || creator() || claimable));
    if level == Level::None && !can_manage {
        return Reply::text(403, "not shared with you");
    }
    let show = |own: Option<access::Entry>| {
        let inherited = acl.effective(key).filter(|(k, _)| k != key);
        json!({
            "owner": own.as_ref().map(|e| e.owner.clone()),
            "view": own.as_ref().map(|e| e.view.clone()),
            "edit": own.as_ref().map(|e| e.edit.clone()),
            "inherited_from": inherited.as_ref().map(|(k, _)| k.clone()),
            "inherited": inherited.as_ref().map(|(_, e)| json!({"owner": e.owner, "view": e.view, "edit": e.edit})),
            "default_open": acl.default_open,
            "can_manage": can_manage,
        })
    };
    let names = |v: &Value, k: &str| -> Option<Vec<String>> {
        v.get(k)?.as_array().map(|a| a.iter().filter_map(|n| n.as_str().map(str::to_string)).collect())
    };
    match method {
        Method::Get => Reply::json(200, &show(own)),
        Method::Post | Method::Delete if !can_manage => Reply::text(403, "only its owner (or an admin) shares it"),
        Method::Post => {
            let Ok(v) = serde_json::from_slice::<Value>(body) else {
                return Reply::text(400, "not a sharing: {\"view\": null|[names], \"edit\": null|[names]}");
            };
            let owner = own.as_ref().map(|e| e.owner.clone()).or_else(|| service.store.first_author(key)).unwrap_or_else(|| user.to_string());
            acl.set(key, access::Entry { owner, view: names(&v, "view"), edit: names(&v, "edit") });
            Reply::json(200, &show(acl.get(key)))
        }
        Method::Delete => {
            acl.remove(key);
            Reply::json(200, &show(None))
        }
        _ => Reply::text(405, "method not allowed"),
    }
}

/// The users requests (`?users`, `?password`, on any folder URL), or `None` for anything else.
/// The list is everyone in the users file plus everyone who ever signed in through the proxy.
fn users_route(service: &Service, admin: bool, method: &Method, url: &str, user: &str, body: &[u8], logout: Option<&str>) -> Option<Reply> {
    let file: &Path = &service.users_file;
    let (path, query) = split_url(url);
    if !path.starts_with("/files/") {
        return None;
    }
    let get = |k: &str| query.iter().find(|(q, _)| q == k).map(|(_, v)| v.as_str());
    // Who am I (the app's account menu): the name, whether an admin, and where Sign out goes.
    if *method == Method::Get && get("me").is_some() {
        return Some(Reply::json(200, &json!({"name": user, "admin": admin, "anonymous": user == ANONYMOUS, "logout": logout})));
    }
    let field = |k: &str| serde_json::from_slice::<Value>(body).ok().and_then(|v| v.get(k).cloned());
    let listing = |status: u16| match users::list(file) {
        Ok(mut list) => {
            // The directory's people first (they may be admins there), then everyone who signed in.
            for p in service.directory.as_ref().map(Directory::users).unwrap_or_default() {
                match list.iter_mut().find(|u| u.name == p.name) {
                    Some(u) => u.admin |= p.admin,
                    None => list.push(users::User { name: p.name, admin: p.admin }),
                }
            }
            let seen = service.seen.lock().map(|s| s.clone()).unwrap_or_default();
            for name in seen.into_iter().filter(|n| n != ANONYMOUS) {
                if !list.iter().any(|u| u.name == name) {
                    list.push(users::User { name, admin: false });
                }
            }
            list.sort_by_key(|u| u.name.to_lowercase());
            Reply::json(
                status,
                &json!({"me": user, "admin": admin, "users": list.iter().map(|u| json!({"name": u.name, "admin": u.admin || u.name == user && admin})).collect::<Vec<_>>()}),
            )
        }
        Err(e) => Reply::text(500, &e),
    };
    let outcome = |r: Result<(), String>| match r {
        Ok(()) => listing(200),
        Err(e) => Reply::text(400, &e),
    };
    Some(match (method, get("users"), get("password")) {
        (Method::Get, Some(_), _) => listing(200),
        (Method::Post, Some(_), _) if !admin => Reply::text(403, "only an admin manages users"),
        (Method::Post, Some(_), _) => {
            let Some(name) = field("name").and_then(|v| v.as_str().map(str::to_string)) else { return Some(Reply::text(400, "a user needs a name")) };
            let password = field("password").and_then(|v| v.as_str().map(str::to_string));
            let make_admin = field("admin").and_then(|v| v.as_bool());
            if name == user && make_admin == Some(false) {
                return Some(Reply::text(400, "you can't take your own admin away"));
            }
            outcome(match (password, make_admin) {
                (Some(password), admin) => users::set(file, &name, &password, admin),
                (None, Some(admin)) => users::set_admin(file, &name, admin),
                (None, None) => Err("a new user needs a password".to_string()),
            })
        }
        (Method::Delete, Some(name), _) if !admin => Reply::text(403, &format!("only an admin removes users ({name})")),
        (Method::Delete, Some(name), _) if name == user => Reply::text(400, "you can't remove yourself"),
        (Method::Delete, Some(name), _) => outcome(users::remove(file, name)),
        (Method::Post, None, Some(_)) => {
            let Some(password) = field("password").and_then(|v| v.as_str().map(str::to_string)) else {
                return Some(Reply::text(400, "a password is needed"));
            };
            outcome(users::set(file, user, &password, None))
        }
        _ => return None,
    })
}

/// A level as the `X-Folder-Access` header says it.
fn level_name(level: Level) -> &'static str {
    match level {
        Level::None => "none",
        Level::View => "view",
        Level::Edit => "edit",
    }
}

/// nginx's autoindex JSON, which the app already reads.
fn listing_json(entries: &[Entry]) -> Value {
    Value::Array(
        entries
            .iter()
            .map(|e| {
                let mtime = httpdate::fmt_http_date(e.mtime);
                if e.dir {
                    json!({"name": e.name, "type": "directory", "mtime": mtime})
                } else {
                    json!({"name": e.name, "type": "file", "mtime": mtime, "size": e.size})
                }
            })
            .collect(),
    )
}

fn versions_json(versions: &[Version]) -> Value {
    Value::Array(versions.iter().map(|v| json!({"hash": v.hash, "author": v.author, "date": v.date, "message": v.message, "size": v.size})).collect())
}

fn changes_json(changes: &[Change]) -> Value {
    Value::Array(changes.iter().map(|c| json!({"hash": c.hash, "author": c.author, "date": c.date, "message": c.message, "path": c.path})).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_and_decode() {
        let (path, query) = split_url("/files/projects/Sample%20Plate.solidcraft?at=abc&history");
        assert_eq!(path, "/files/projects/Sample Plate.solidcraft");
        assert_eq!(query, vec![("at".to_string(), "abc".to_string()), ("history".to_string(), String::new())]);
    }
}
