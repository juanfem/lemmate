# lemmate-desktop

The Tauri 2 desktop shell (SPEC §3.1, §14). It is deliberately thin: it starts the engine's
**local relay** for every vault the account can read and opens **one window** on the URL that
relay serves.

```
lemmate-desktop ──> lemmate_core::client::start_many(Vec<SyncOptions>, LocalOptions)
                     │  binds 127.0.0.1:<stable port>, serves ui/dist + the local API,
                     │  one engine per vault — folder, sidecar, watcher, connection —
                     │  all behind one relay, frames routed by doc id
                     └──> window at http://127.0.0.1:<port>/
```

Each vault gets its own folder under the root:

```
<root_dir>/
  Work/            ← a vault named "Work"
    .lemmate/      ← its sidecar: local.db, attachments
  vault-3f9c2a/    ← a vault with no name yet; renamed to its name on the next launch
```

Which vaults those are comes from the server (`GET /api/v1/vaults`) on each launch; folders
already on disk open whether or not the server answers, which is what makes the app work
offline. The folder ↔ vault binding is the sidecar, never the folder name, so a folder you
rename yourself keeps working and is never renamed back.

**A server is optional** (SPEC §3.2). Leave `server_url` out and the shell runs *standalone*:
the folders under the root are the vaults, no connection is made, and the relay answers
everything the UI asks — search, backlinks, tags, trash, history, daily notes, import, export,
attachments — from the sidecar, as it already does offline. Sharing and accounts are the
server's, so the UI does not offer them; the status line says `local` instead of `online`.

**Merging vaults** (*Merge a vault into another…*) is the relay's, not this crate's: it is the
one thing that holds two engines at once, so `POST /api/v1/local/merge` surveys both, plans the
move, copies the files across and retires the source — which deletes itself on the server too if
it had one. The shell only notices that a vault it opened has gone: the engine's task ends, and
`vaults::plan` finds one fewer folder on the next launch.

**Connecting one later** is a command in the UI (*Connect a server…*), and this crate is the
half that answers it. The relay carries the request out through `LocalHandle::connect` — it only
offers the endpoint at all when `LocalOptions::config_path` names a file — and `watch_for_connect`
signs in, asks the server which vaults the account can read (proving the URL, the token and the
CA before anything is written), records the server in `desktop.toml` with `Config::set_server`,
and calls `AppHandle::restart`. The engines were built from the old configuration and cannot be
given a server in place; the restart keeps the same arguments, so the app comes back on the file
it just wrote. The HTTP answer goes out half a second before the process does, so the dialog
that asked shows "connected" rather than a dropped connection.

**New vault** in the tree mints its id in the browser and speaks it over the socket — there is
no REST call to intercept — so the relay holds those frames, opens a folder and an engine for
the vault, and carries on. That is what `vault_root` in `LocalOptions` is for; a shell pointed
at a single vault leaves it unset and frames for any other vault are dropped, as before.

Nothing is exposed to the webview over Tauri IPC — the UI talks to the relay over plain
HTTP/WebSocket, exactly as the web client talks to the server. The `LocalHandle` is kept in
Tauri managed state and `abort()`ed on `RunEvent::Exit`.

## Configuration

`desktop.toml` in the per-user configuration directory — `~/.config/lemmate` on Linux
(honouring `$XDG_CONFIG_HOME`), `~/Library/Application Support/lemmate` on macOS,
`%APPDATA%\lemmate` on Windows, or `$LEMMATE_CONFIG_DIR` when that is set:

```toml
root_dir   = "/home/you/lemmate"            # required — one folder per vault goes in here
server_url = "https://notes.example.org"    # optional — omit to run standalone
ca_cert    = "/etc/ssl/private-ca.pem"      # optional: trust a private CA for wss:// / https://
web_dir    = "/path/to/ui/dist"             # optional: override the web assets the relay serves
```

To open a **single** vault instead of the workspace — a folder that is already a vault, or one
you want on its own — give `vault_dir` in place of `root_dir`, optionally with a `vault_id` to
join an existing vault. That is also what a `desktop.toml` written before roots existed says, so
those keep working unchanged, and the window then opens on `#/v/<vault-id>` as it always did.

Every key has a flag that overrides it, and most have an environment variable:

| Flag | Env | Key |
|---|---|---|
| `--config FILE` | `LEMMATE_DESKTOP_CONFIG` | — (which file to read) |
| `--root-dir DIR` | `LEMMATE_ROOT_DIR` | `root_dir` |
| `--vault-dir DIR` | `LEMMATE_VAULT_DIR` | `vault_dir` (instead of a root) |
| `--server-url URL` | `LEMMATE_SERVER` | `server_url` |
| `--vault-id ULID` | — | `vault_id` |
| `--ca-cert FILE` | `LEMMATE_CA_CERT` | `ca_cert` |
| `--web-dir DIR` | `LEMMATE_WEB_DIR` | `web_dir` |

Without a usable configuration — meaning no folder for the notes, with or without a server —
the app opens a **setup screen** in the window (notes folder, and *Sync with a server* with the
URL and optional account behind it) and writes this file for you; the flags above still override
it. The screen never asks for a vault id: which vaults you have is the server's answer, not
yours to type, and standalone it is whatever is under the root.

## Running in development

```sh
(cd ui && npm install && npm run build)     # the relay serves ui/dist; build it first
cargo run -p lemmate-desktop -- --root-dir /path/to/notes            # standalone

cargo run -p lemmate-server -- --data-dir ./data                     # …or with a server
cargo run -p lemmate-desktop -- --root-dir /path/to/notes --server-url http://127.0.0.1:8080

RUST_LOG=info cargo run -p lemmate-desktop    # …with the config file instead of flags
```

`lemmate serve --root /path/to/notes --web-dir ui/dist` is the same standalone relay without
the Tauri window, which is the quickest way to exercise it in a browser.

## Web assets

The relay needs a directory of built web assets. They are resolved in this order:

1. `--web-dir` / `LEMMATE_WEB_DIR` / `web_dir` in the config file;
2. `<resource dir>/ui/dist` — the copy shipped by `bundle.resources` in `tauri.conf.json`
   (`"../../ui/dist/": "ui/dist/"`), used in an installed build;
3. `<repo>/ui/dist` relative to `CARGO_MANIFEST_DIR` — dev mode, straight from the source tree.

`ui/dist` is gitignored, so `npm run build` in `ui/` must have run before `cargo tauri build`
packages it. `cargo check`/`cargo build`/`cargo run` do not need the *assets* — only the relay
reads them, at startup, and then only if you have not passed `--web-dir` — but they do need the
**directory to exist**: `tauri-build` resolves `bundle.resources` and fails the build script with
"resource path `../../ui/dist` doesn't exist" otherwise. An empty `mkdir -p ui/dist` is enough,
which is what CI does for its type-check jobs.

`build.frontendDist` points at the small placeholder in `frontend/`, **not** at `ui/dist`.
Tauri requires `frontendDist` to exist when the crate is compiled, and this shell never loads
it — the window opens on an external `http://127.0.0.1:<port>` URL — so pointing it at a
committed placeholder keeps the crate buildable without a prior UI build and keeps ~5 MB of
assets out of the binary. The real assets ship as a bundle resource instead.

## Icons

`icons/` holds **placeholder** icons (a dark rounded square with three lines), generated with
Pillow because `cargo tauri icon` is not installed here. Replace them with the real artwork:

```sh
cargo install tauri-cli --version '^2'
cargo tauri icon path/to/icon.png     # run from crates/desktop
```

## Notes

- Tray icons are not enabled (`tauri`'s `tray-icon` feature is off), so libayatana-appindicator
  is not required.
- Linux build dependencies: `webkit2gtk-4.1`, `javascriptcoregtk-4.1`, `libsoup-3.0`, `gtk+-3.0`.
- `gen/schemas/` is generated by `tauri-build` and is gitignored.
