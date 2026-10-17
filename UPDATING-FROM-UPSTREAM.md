# Updating this fork from upstream

This checkout (`W:\solidcraft`, `/mnt/Apps/Apps/solidcraft` on the NAS) is a fork of
[tangency-labs/solidcraft](https://github.com/tangency-labs/solidcraft). The `server-storage`
branch carries one self-hosting patch on top of upstream `main`: the web app keeps designs on
the server instead of in the browser.

## What the patch changes

| File | Change |
| --- | --- |
| `apps/solidcraft-web/src/files.rs` | A third storage backend, "remote": at start the app probes `files/` (same origin). A JSON listing there means designs are listed, read (`GET`), written (`PUT`) and deleted (`DELETE`) on the server. A 404 means the upstream behaviour (OPFS / local storage) is unchanged. The autosave (`recovery.*`) always stays in the browser. |
| `apps/solidcraft-web/Cargo.toml` | Three more `web-sys` features (`RequestInit`, `RequestCache`, `Headers`). |
| `packaging/docker/nginx.conf` | `location ^~ /files/`: nginx's JSON autoindex plus `dav_methods PUT DELETE`, backed by `/srv/designs`. |

Outside the repo, `W:\docker-compose.yml` mounts `/mnt/Apps/Apps/SolidCraftDesigns` (that is
`W:\SolidCraftDesigns` on the PC) at `/srv/designs` in the `solidcraft` service. Keep only
designs in that folder: everything in it is listed in the app and read at start.

Remotes: `origin` is the fork (github.com/dmquinny/solidcraft), `upstream` is tangency-labs.

## Pull upstream into the branch

From `W:\solidcraft` (Git Bash or PowerShell):

```bash
git fetch upstream
git checkout server-storage
git merge upstream/main
```

A merge usually goes through untouched: upstream rarely edits the three primitives the patch
hooks into. If `files.rs` conflicts, keep upstream's version of everything except the pieces
marked "self-hosting patch" (the module-doc paragraph, the `REMOTE` block after `local_only`,
the two `if remote_only() && !stays_local(name)` guards, and `read_all` / `read_browser`),
then:

```bash
git add -A
git merge --continue
```

Optional local check (needs the wasm target: `rustup target add wasm32-unknown-unknown`):

```bash
cargo check -p solidcraft-web --target wasm32-unknown-unknown
```

Then push the branch to the fork:

```bash
git push origin server-storage
```

Keeping the fork's `main` in step with upstream is optional (`git push origin upstream/main:main`).

## Rebuild on the NAS

Docker on TrueNAS is root-only. In the web shell (System › Shell at https://192.168.1.2:8445)
run `sudo -s`, then:

```bash
cd /mnt/Apps/Apps && docker compose --progress=plain up -d --build solidcraft 2>&1 | tee /mnt/Apps/Apps/solidcraft-build.log
```

The first build takes several minutes; rebuilds reuse the cargo cache mount. Reload
https://solidcraft.lan.homedc.xyz afterwards (the service worker picks up the new build on the
next launch). Check that `GET https://solidcraft.lan.homedc.xyz/files/` still answers a JSON
list: that is what switches the app to server storage.

## Going back to upstream behaviour

Remove the `volumes:` entry from the `solidcraft` service in `W:\docker-compose.yml` and
rebuild: with no folder at `/srv/designs`, nginx answers 404 on `files/` and the app keeps
designs in the browser again. Or check out `upstream/main` and rebuild for a plain upstream
build.
