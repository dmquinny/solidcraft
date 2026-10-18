//! Design storage for the self-hosted SolidCraft web app (a self-hosting patch, not upstream).
//!
//! The web app keeps designs on the server through a small HTTP contract under `files/`
//! (apps/solidcraft-web/src/files.rs): `GET files/<folder>/` lists a folder as JSON, `GET`,
//! `PUT` and `DELETE files/<path>` read, write and remove one design. This service answers that
//! contract from a folder on disk and keeps every change in a git repository next to it, so the
//! app can show who saved what and when, and put an earlier version back.
//!
//! nginx sits in front (packaging/docker/nginx.conf): it signs users in (`auth_basic` against
//! the users file this binary maintains) and passes the name in `X-Remote-User`. The service
//! itself is reachable only on the container network, so it trusts that header.
//!
//! Routes (all under `/files/`; paths percent-encoded, `..` and hidden names refused):
//! - `GET /files/<folder>/` → the folder's entries; `?log` → its recent changes (who, what, when).
//!   The signed-in user comes back in `X-Remote-User`.
//! - `GET /files/<path>` → the design's bytes, `ETag`, `Last-Modified`, `X-Author`;
//!   `?history` → its versions; `?at=<hash>` → the bytes of that version.
//! - `PUT /files/<path>` → 201 / 204 with the new `ETag`; with `If-Match`, 412 and
//!   `{"author","date"}` when someone else saved since that version; with `If-None-Match: *`,
//!   412 when the design exists already.
//! - `GET /files/?shared` → what others shared with the signed-in user by name (path, folder?,
//!   owner, may edit?).
//! - `DELETE /files/<path>` → 204 (404 when absent).
//! - `POST /files/<path>?restore=<hash>` → that version becomes the current one (a new commit).
//! - Live collaboration (see `ops`): `POST /files/<path>?ops` appends the commands a client ran
//!   (`{"client", "ops": [{"id", "params", "live", "after"}]}`); `GET /files/<path>?ops&since=N
//!   [&wait=S&client=C]` answers the entries after N (waiting up to S seconds for some), the
//!   latest saved marker, who else is there and who holds what. A design's `GET` carries
//!   `X-Ops-Seq` and `X-Ops-Commit`: the marker its bytes correspond to. `POST ?lock=<component>
//!   &client=C` holds a component (0 = the whole design) for the user, 409 when someone else
//!   does; `POST ?unlock=<component>` lets it go; commands touching a component someone else
//!   holds are refused with 409. `POST /files/<path>?branch=<new path>` copies the design under
//!   the new name, whose log's first marker says where it came from (merging is the client's:
//!   it replays the branch's commands onto the target).
//! - Users: `GET /files/?users` → who is signed in, whether they are an admin, and everyone's
//!   names; `POST /files/?users` `{"name","password","admin"}` adds a user or gives one a new
//!   password (admins only); `DELETE /files/?users=<name>` removes one (admins only, never the
//!   last admin); `POST /files/?password` `{"password"}` changes the caller's own.
//! - Sharing (`access`): `GET /files/<path>?access` (a folder: with the trailing slash) → who
//!   owns it, who may view and edit it, what it inherits; `POST` `{"view": null|[names],
//!   "edit": null|[names]}` sets it (owners, admins, and the creator of a design that has no
//!   entry yet); `DELETE` removes its own entry so it follows its folder again. Requests for a
//!   design someone may not view answer 403, and so do changes from someone who may only view.
//!   `SOLIDCRAFT_DEFAULT_ACCESS` is `everyone` (the default) or `owner` (new designs start as
//!   their creator's own).
//! - `GET /healthz` → 200.
//!
//! Without a name from nginx a request is refused (401), unless `SOLIDCRAFT_ALLOW_ANONYMOUS=1`.
//! A proxy that signed the user in may also pass their groups (`X-Remote-Groups`, comma
//! separated): members of `SOLIDCRAFT_ADMIN_GROUP` (default `solidcraft-admins`) are admins
//! too. Every name that ever signed in is remembered (`<history>/users-seen`), so the Users
//! page lists people the proxy knows even when the users file does not. With
//! `SOLIDCRAFT_AUTHENTIK_URL` and `SOLIDCRAFT_AUTHENTIK_TOKEN` the whole directory is read from
//! Authentik's API (`directory`), so everyone appears before they ever sign in, and Authentik's
//! superusers and the admin group's members are admins; a name that is an email (what Cloudflare
//! Access passes) becomes the directory's user name.
//!
//! Command line: `solidcraft-server` serves; `solidcraft-server user add|remove|list [NAME]`
//! maintains the users file. Settings come from the environment: `SOLIDCRAFT_DESIGNS`
//! (default /srv/designs), `SOLIDCRAFT_HISTORY` (default /srv/history: the git repository and
//! the users file), `SOLIDCRAFT_LISTEN` (default 0.0.0.0:8782).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod access;
mod directory;
mod http;
mod ops;
mod store;
mod users;

use std::path::PathBuf;

/// Where things are, from the environment.
pub struct Settings {
    pub designs: PathBuf,
    pub history: PathBuf,
    pub listen: String,
    /// Serve requests that carry no user name (nginx without sign-in).
    pub allow_anonymous: bool,
    /// The group (in `X-Remote-Groups`) whose members are admins.
    pub admin_group: String,
    /// Designs without sharing of their own are everyone's (else their creator's).
    pub default_open: bool,
    /// Authentik's address and an API token that may read its users (both empty: no directory).
    pub authentik_url: String,
    pub authentik_token: String,
    /// Where the app's Sign out goes when the proxy doesn't say (`X-Remote-Logout`); empty: nowhere.
    pub logout_url: String,
}

impl Settings {
    fn from_env() -> Self {
        let var = |k: &str, d: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| d.to_string());
        Self {
            designs: PathBuf::from(var("SOLIDCRAFT_DESIGNS", "/srv/designs")),
            history: PathBuf::from(var("SOLIDCRAFT_HISTORY", "/srv/history")),
            listen: var("SOLIDCRAFT_LISTEN", "0.0.0.0:8782"),
            allow_anonymous: matches!(var("SOLIDCRAFT_ALLOW_ANONYMOUS", "0").as_str(), "1" | "true" | "yes"),
            admin_group: var("SOLIDCRAFT_ADMIN_GROUP", "solidcraft-admins"),
            default_open: var("SOLIDCRAFT_DEFAULT_ACCESS", "everyone") != "owner",
            authentik_url: var("SOLIDCRAFT_AUTHENTIK_URL", ""),
            authentik_token: var("SOLIDCRAFT_AUTHENTIK_TOKEN", ""),
            logout_url: var("SOLIDCRAFT_LOGOUT_URL", ""),
        }
    }

    /// nginx's `auth_basic_user_file`.
    pub fn users_file(&self) -> PathBuf {
        self.history.join("htpasswd")
    }

    /// Every name that ever signed in, one per line.
    pub fn seen_file(&self) -> PathBuf {
        self.history.join("users-seen")
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).format_timestamp_secs().init();
    let settings = Settings::from_env();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None | Some("serve") => http::serve(&settings),
        Some("user") => users::command(&settings.users_file(), &args[1..]),
        Some("health") => health(&settings),
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command {other}\n{USAGE}")),
    };
    if let Err(e) = result {
        eprintln!("solidcraft-server: {e}");
        std::process::exit(1);
    }
}

/// Is the service answering? (The container's health check.)
fn health(settings: &Settings) -> Result<(), String> {
    use std::io::{BufRead, Write};
    let addr = settings.listen.replace("0.0.0.0", "127.0.0.1").replace("[::]", "[::1]");
    let mut s = std::net::TcpStream::connect(&addr).map_err(|e| format!("{addr}: {e}"))?;
    s.set_read_timeout(Some(std::time::Duration::from_secs(5))).map_err(|e| e.to_string())?;
    s.write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").map_err(|e| e.to_string())?;
    let mut status = String::new();
    std::io::BufReader::new(s).read_line(&mut status).map_err(|e| e.to_string())?;
    if status.starts_with("HTTP/1.1 200") || status.starts_with("HTTP/1.0 200") { Ok(()) } else { Err(format!("{addr}: {}", status.trim())) }
}

const USAGE: &str = "usage: solidcraft-server [serve|health]
       solidcraft-server user add NAME [--admin] [--password-stdin]
       solidcraft-server user remove NAME
       solidcraft-server user list";
