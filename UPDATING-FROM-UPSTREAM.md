# Updating this fork from upstream

This checkout (`W:\solidcraft`, `/mnt/Apps/Apps/solidcraft` on the NAS) is a fork of
[tangency-labs/solidcraft](https://github.com/tangency-labs/solidcraft). The `server-storage`
branch carries one self-hosting patch on top of upstream `main`: the web app keeps designs on
the server instead of in the browser, optionally in a subfolder chosen in the file panel.

## What the patch changes

| File | Change |
| --- | --- |
| `apps/solidcraft-web/src/files.rs` | A third storage backend, "remote": at start the app probes `files/` (same origin). A JSON listing there means designs are listed, read (`GET`), written (`PUT`) and deleted (`DELETE`) on the server. A 404 means the upstream behaviour (OPFS / local storage) is unchanged. The autosave (`recovery.*`) always stays in the browser. Designs can be filed in a subfolder (`set_remote_folder`, `remote_folder`): the browser remembers it in local storage (`solidcraft.remote_folder`), the listing's directory entries are offered as subfolders, and a folder the server hasn't got yet lists as empty until the first save makes it. |
| `apps/solidcraft-web/src/main.rs` | A few lines in `file_panel` (and a `folder` field on `Shell`): with server storage the window is titled "Files on the server", shows a folder field with buttons for the subfolders, and its spinner and empty-list wording say "server" instead of "browser". |
| `apps/solidcraft-web/Cargo.toml` | Three more `web-sys` features (`RequestInit`, `RequestCache`, `Headers`). |
| `packaging/docker/nginx.conf` | `location ^~ /files/`: nginx's JSON autoindex plus `dav_methods PUT DELETE` (with `create_full_put_path on`, so a subfolder appears on the first save into it), backed by `/srv/designs`. |

Outside the repo, `W:\docker-compose.yml` mounts `/mnt/Apps/Backup/Maker/SolidCraft` (that is
`Z:\Maker\SolidCraft` on the PC, a Syncthing-synced folder) at `/srv/designs` in the `solidcraft` service. Keep only
designs in that folder: everything in it is listed in the app and read at start. Subfolders (made
from the file panel's folder field, or by hand) are listed as folders, each with its own designs.

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
marked "self-hosting patch" (the module-doc paragraph, the `REMOTE` block after `local_only`
through `remote_remove`, the two `if remote_only() && !stays_local(name)` guards, `read_all` /
`read_remote` / `read_browser`, and `start` / `install` / `set_remote_folder`). If `main.rs`
conflicts, keep upstream's version and re-apply the `remote` lines in `file_panel` (the title,
the `if remote` folder row, the two `if remote` labels, the `"folder"` action) and the `folder`
field of `Shell`. Then:

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
