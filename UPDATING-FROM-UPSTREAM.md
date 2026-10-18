# Updating this fork from upstream

This checkout (`W:\solidcraft`, `/mnt/Apps/Apps/solidcraft` on the NAS) is a fork of
[tangency-labs/solidcraft](https://github.com/tangency-labs/solidcraft). Two branches carry
self-hosting patches on top of upstream `main`:

- `server-storage`: the web app keeps designs on the server instead of in the browser (nginx
  WebDAV), optionally in a subfolder chosen in the file panel. A small patch.
- `collab` (on top of `server-storage`, the branch deployed now): a storage service of its own
  (`apps/solidcraft-server`) answers `files/` instead of nginx: everyone signs in, every save is
  recorded with the user as author, the panel shows who changed what, each design's history,
  and can restore or copy an earlier version; a save over someone else's newer save is refused
  once. Live collaboration: people with the same server design open see each other's edits as
  they happen (the commands they run go through a log on the server, in one order for everyone).
  A user can lock a component (or the whole design) so only they change it. A design can be
  branched (a copy that remembers where it came from) and merged back by replaying its edits.

## What the patch changes

| File | Change |
| --- | --- |
| `apps/solidcraft-web/src/files.rs` | A third storage backend, "remote": at start the app probes `files/` (same origin). A JSON listing there means designs are listed, read (`GET`), written (`PUT`) and deleted (`DELETE`) on the server. A 404 means the upstream behaviour (OPFS / local storage) is unchanged. The autosave (`recovery.*`) always stays in the browser. Designs can be filed in a subfolder (`set_remote_folder`, `remote_folder`): the browser remembers it in local storage (`solidcraft.remote_folder`), the listing's directory entries are offered as subfolders, and a folder the server hasn't got yet lists as empty until the first save makes it. |
| `apps/solidcraft-web/src/main.rs` | A few lines in `file_panel` (and a `folder` field on `Shell`): with server storage the window is titled "Files on the server", shows a folder field with buttons for the subfolders, and its spinner and empty-list wording say "server" instead of "browser". |
| `apps/solidcraft-web/Cargo.toml` | Three more `web-sys` features (`RequestInit`, `RequestCache`, `Headers`). |
| `packaging/docker/nginx.conf` | `location ^~ /files/`: nginx's JSON autoindex plus `dav_methods PUT DELETE` (with `create_full_put_path on`, so a subfolder appears on the first save into it), backed by `/srv/designs`. |

And on `collab`:

| File | Change |
| --- | --- |
| `apps/solidcraft-server/` | A new binary crate (plain threads, `tiny_http`, git through the `git` command): the `files/` contract plus `X-Remote-User` on listings, `?log` (a folder's recent changes), `?history` and `?at=<hash>` (a design's versions), `POST ?restore=<hash>`, `ETag` / `If-Match` / `If-None-Match: *` on saves (412 with the other author's name and time). The designs folder is the work tree of a git repository kept in `/srv/history/designs.git`; changes made outside the app are committed as "disk". `solidcraft-server user add NAME` keeps nginx's users file (`/srv/history/htpasswd`, `{SSHA}`). |
| `packaging/docker/Dockerfile.server` | The service's image (Debian slim with git). |
| `packaging/docker/nginx.conf` | `auth_basic` for the whole site against that users file; `/files/` proxied to `solidcraft-server:8782` with `X-Remote-User`; `/healthz` without sign-in for the health check. |
| `apps/solidcraft-web/src/files.rs` | Saves carry the version they replace; a 412 is reported with the other person's name and the next save overwrites. The signed-in name, a folder's recent changes, a design's versions, restore and "copy a version" (`history`, `restore`, `copy_version`, `remote_user`, `ago`, `when`). |
| `apps/solidcraft-web/src/main.rs` | The user in the panel's title, a History button per design, a "Recent changes" list, and the history window (`history_window`). The `Shell` drives `sync.rs` once a frame and shows "Also here: …" while others have the design open. |
| `crates/engine/src/sync.rs` (new), `crates/engine/src/lib.rs` | The one change below the apps: a `shared` flag on `Session`, and three lines in `Session::execute` that record each outermost command that changed the design of a shared session (`sync::record`), unless it is being applied on another session's behalf (`sync::apply`). `sync::fingerprint` digests a document. `doc.*`, `file.*` and the checkpoint commands are never shared. |
| `apps/solidcraft-server/src/ops.rs` | One log per design (`<history>/ops/<design>.jsonl`): the commands clients ran, and a marker for every saved version (etag, commit, the saver's fingerprint). `POST ?ops` appends; `GET ?ops&since=N&wait=S&client=C` answers what came after N, waiting up to S seconds, with who else is polling. A design's `GET`/`PUT` carry `X-Ops-Seq` / `X-Ops-Commit`, the marker its bytes correspond to. |
| `apps/solidcraft-web/src/sync.rs` (new) | The client side: the open server design is marked `shared`; commands run here are batched and posted; others' commands come back through a long poll and are applied with `sync::apply`; the client's own entries are skipped by its id. A command that fails to apply, or a fingerprint that disagrees with the one another session reported (after its command, or with its save), makes the session start afresh: load the design's last saved version and replay the log after it. Status messages say who joined, who left and when a catch-up happened. Locks come with every poll and go to the engine (`sync::set_locks`); `lock`/`unlock` hold the active component; `merge` fetches a branch's commands and replays them onto the target as this user's edits. |
| Locks (`crates/engine/src/sync.rs`, `apps/solidcraft-server/src/ops.rs`) | `sync::touched` says which components a command changed (0 = the design as a whole: parameters, components, placements, joints, settings); `Session::execute` refuses, and undoes, a change to a component someone else holds ("component X is locked by felix"), the server refuses such commands too (409). `POST ?lock=<component>&client=C` / `?unlock=` hold and release; the holder's polls renew the lock, which lapses two minutes after they stop. The live panel (top right while a shared design is open) shows who else is here and holds or releases the active component. |
| Branches (`store.rs`, `files.rs`, `sync.rs`) | `POST ?branch=<new path>` copies the design (commit "branched … from …"); the branch's first marker carries `from`. "Branch as" in the history window makes one in the current folder; "Merge into" fetches the branch's whole command log and replays it onto the chosen design, which is opened and left unsaved; commands that no longer apply are counted and skipped. |

Outside the repo, `W:\docker-compose.yml` runs two services: `solidcraft` (nginx, the site) and
`solidcraft-server` (the storage service). The designs folder `/mnt/Apps/Backup/Maker/SolidCraft`
(that is `Z:\Maker\SolidCraft` on the PC, a Syncthing-synced folder) is mounted at `/srv/designs`
in the service; `/mnt/Apps/Apps/solidcraft-history` (owned by 1000:1000) holds the git history
and the users file and is mounted at `/srv/history` in both (read-only in nginx). Keep only
designs in the designs folder: everything in it is listed in the app and read at start.
Subfolders (made from the file panel's folder field, or by hand) are listed as folders, each with
its own designs. Hidden names (starting with a dot, such as Syncthing's markers) are ignored and
never committed.

How the live sync behaves, in short: edits to different features, sketches or components merge
on their own; two people editing the very same thing at the same moment means one of them gets
a refused command (its status line says so) and their session catches up with the server's
order. Every save is a checkpoint: a session that disagrees with a save reloads from it. Undo
and redo are shared like any other command. The log is per design and per browser tab; a design
that never came from the server (new, unsaved) is shared once it is saved there. Locks are the
way to avoid stepping on each other in the same component: lock it, work, unlock. Branches are
for longer detours: branch, work on the copy (alone or together), merge back when done; a merge
is a replay, so edits that no longer fit the target are skipped and reported.

Users: in the TrueNAS root shell, `cd /mnt/Apps/Apps && docker compose exec -it solidcraft-server
solidcraft-server user add NAME` (it asks for the password twice); `user list` and `user remove NAME`
likewise. The site asks for a sign-in until at least one user exists. Browsers remember the
sign-in for the session; closing the browser signs out.

Remotes: `origin` is the fork (github.com/dmquinny/solidcraft), `upstream` is tangency-labs.

## Pull upstream into the branches

From `W:\solidcraft` (Git Bash or PowerShell), first the small branch, then `collab` on top of it:

```bash
git fetch upstream
git checkout server-storage
git merge upstream/main
git checkout collab
git merge server-storage
```

A merge usually goes through untouched: upstream rarely edits the three primitives the patch
hooks into. If `files.rs` conflicts, keep upstream's version of everything except the pieces
marked "self-hosting patch" (the module-doc paragraph, the `REMOTE` block after `local_only`
through `remote_remove`, the two `if remote_only() && !stays_local(name)` guards, `read_all` /
`read_remote` / `read_browser`, and `start` / `install` / `set_remote_folder`). If `main.rs`
conflicts, keep upstream's version and re-apply the `remote` lines in `file_panel` (the title,
the `if remote` folder row, the two `if remote` labels, the `"folder"` and `"history"` actions),
`history_window`, and the `folder` and `sync` fields of `Shell` with the two `sync` lines in
`update`. If `crates/engine/src/lib.rs` conflicts, keep upstream's version and re-add `pub mod
sync;`, the `shared` field (its two `shared: false` initialisers) and the three `sync::record`
lines in `execute`. Then:

```bash
git add -A
git merge --continue
```

Optional local check (needs the wasm target: `rustup target add wasm32-unknown-unknown`):

```bash
cargo check -p solidcraft-web --target wasm32-unknown-unknown
```

On `collab` also the service (it runs on this PC too, against any folder, for a quick look):

```bash
cargo test -p solidcraft-server
```

Then push the branches to the fork:

```bash
git push origin server-storage collab
```

Keeping the fork's `main` in step with upstream is optional (`git push origin upstream/main:main`).

## Rebuild on the NAS

Docker on TrueNAS is root-only. In the web shell (System › Shell at https://192.168.1.2:8445)
run `sudo -s`, then:

```bash
cd /mnt/Apps/Apps && docker compose --progress=plain up -d --build solidcraft solidcraft-server 2>&1 | tee /mnt/Apps/Apps/solidcraft-build.log
```

The first build takes several minutes; rebuilds reuse the cargo cache mount. Reload
https://solidcraft.lan.homedc.xyz afterwards (the service worker picks up the new build on the
next launch). Check that `GET https://solidcraft.lan.homedc.xyz/files/` still answers a JSON
list: that is what switches the app to server storage.

## Going back

To the small `server-storage` build (nginx WebDAV, no sign-in, no history): check out
`server-storage`, drop the `solidcraft-server` service and the `/srv/history` mount and the
`healthcheck:` override from `W:\docker-compose.yml`, put the designs mount back on the
`solidcraft` service, and rebuild. The git history in `/mnt/Apps/Apps/solidcraft-history` keeps
every version saved so far and can be read with plain git
(`git --git-dir=/mnt/Apps/Apps/solidcraft-history/designs.git log`).

To upstream behaviour: check out `upstream/main` and rebuild for a plain upstream build; with no
folder at `/srv/designs`, nginx answers 404 on `files/` and the app keeps designs in the browser.
