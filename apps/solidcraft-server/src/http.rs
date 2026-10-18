//! The HTTP side: routes under `/files/` onto the store (see the crate doc for the contract).

use std::io::Read;
use std::sync::Arc;

use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::Settings;
use crate::store::{Change, Entry, Meta, Store, Version, Written, clean_path};

/// The largest design accepted (nginx has the same limit).
const MAX_BODY: usize = 200 << 20;
/// Entries of a folder's change log.
const LOG_ENTRIES: usize = 30;
/// nginx passes the signed-in name here; without nginx (or auth) everyone is this.
const USER_HEADER: &str = "X-Remote-User";
const ANONYMOUS: &str = "anonymous";

pub fn serve(settings: &Settings) -> Result<(), String> {
    let store = Arc::new(Store::open(&settings.designs, &settings.history)?);
    let server = Server::http(&settings.listen).map_err(|e| format!("listen on {}: {e}", settings.listen))?;
    log::info!("serving {} on http://{}/files/ (history in {})", settings.designs.display(), settings.listen, settings.history.display());
    for request in server.incoming_requests() {
        let store = store.clone();
        std::thread::spawn(move || handle(&store, request));
    }
    Ok(())
}

fn handle(store: &Store, mut request: Request) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let user = header(&request, USER_HEADER).filter(|u| !u.is_empty()).unwrap_or_else(|| ANONYMOUS.to_string());
    let if_match = header(&request, "If-Match").map(|v| v.trim().trim_start_matches("W/").trim_matches('"').to_string());
    let only_new = header(&request, "If-None-Match").is_some_and(|v| v.trim() == "*");
    let body = if method == Method::Put { read_body(&mut request) } else { Ok(Vec::new()) };
    let reply = match body {
        Ok(body) => route(store, &method, &url, &user, if_match.as_deref(), only_new, body),
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

fn route(store: &Store, method: &Method, url: &str, user: &str, if_match: Option<&str>, only_new: bool, body: Vec<u8>) -> Reply {
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
    store.sweep(false);
    let result = match (method, folder) {
        (Method::Get, true) if get("log").is_some() => {
            store.log(&clean, LOG_ENTRIES).map(|c| Reply::json(200, &changes_json(&c)).with(USER_HEADER, user))
        }
        (Method::Get, true) => store.list(&clean).map(|l| match l {
            Some(entries) => Reply::json(200, &listing_json(&entries)).with(USER_HEADER, user),
            None => Reply::text(404, "no such folder"),
        }),
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
        (Method::Put, false) => store.write(&clean, &body, user, if_match, only_new).map(|w| match w {
            Written::Created(tag) => Reply::new(201).with_meta(&Meta { etag: tag, ..Default::default() }),
            Written::Updated(tag) | Written::Unchanged(tag) => Reply::new(204).with_meta(&Meta { etag: tag, ..Default::default() }),
            Written::Stale(meta) => Reply::json(412, &json!({"author": meta.author, "date": meta.date})).with_meta(&meta),
        }),
        (Method::Delete, false) => store.delete(&clean, user).map(|gone| if gone { Reply::new(204) } else { Reply::text(404, "no such design") }),
        (Method::Post, false) if get("restore").is_some() => store.restore(&clean, get("restore").unwrap_or(""), user).map(|r| match r {
            Some(tag) => Reply::new(200).with_meta(&Meta { etag: tag, ..Default::default() }),
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
