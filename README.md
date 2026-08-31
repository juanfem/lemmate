# Lemmate

Self-hosted, open-source, multi-user markdown notes. See [SPEC.md](SPEC.md) for the full
specification; this README covers the repository and the current milestone.

## Status: M1 — single-user desktop MVP (in progress)

| Crate / package | Path | What it is |
|---|---|---|
| `lemmate-core` | `crates/core` | Shared engine: yrs CRDT docs (note + vault), text-diff application, SQLite update log + snapshots + FTS, on-disk projection and external-edit ingestion, watcher, markdown indexer, attachments, TLS, the client sync engine (`client::run`) and its **local relay** (`client::start`), Obsidian import, zip export. |
| `lemmate-server` | `crates/server` | axum: WebSocket sync relay with persistence and retention policy, derived notes/tags/FTS, content-addressed attachments with orphan purge, accounts/sessions/vault roles enforced on REST and the relay, REST (`/api/v1`), serves the web client. |
| `lemmate-cli` | `crates/cli` | `lemmate` binary: `login`/`logout`/`passwd`/`invite`, `sync` (with `--serve` relay), remote `vaults/ls/cat/new/edit/mv/rm/daily/find/backlinks/tags`, `mcp` (Model Context Protocol server over stdio), `index`, `search`, `import obsidian`, `export zip`, `doctor` — see [crates/cli/README.md](crates/cli/README.md). |
| `lemmate-desktop` | `crates/desktop` | Tauri 2 shell: starts the relay for the configured vault and opens one window on it. |
| `lemmate-ui` | `ui/` | Svelte 5 + CodeMirror 6 client: live preview (headings, emphasis, code, links, wikilinks/embeds, math, tags, tasks, quotes, callouts, tables, folded front matter), `[[`/`#` autocomplete, **every vault in one tree**, tabs, quick switcher, command palette, cross-vault search, tags, outline, backlinks, bookmarks, history, daily notes + templates, paste/drop attachments, Obsidian import, sharing (users, public links), presence, login. Also the markdown indexer sharing `corpus/` with `lemmate-core`. |
| corpus | `corpus/` | Markdown conformance cases both indexers must satisfy. |

M0, M1 and M2 are complete (split panes and the desktop setup screen included); see
`docs/deploy.md` for Docker and fly.io. M3 so far: pandoc export, REST/relay writes, MCP server,
remote CLI, the all-vaults workspace and Obsidian import from the UI; mobile and Quarto
rendering remain.

Verification: `cargo test --workspace --exclude lemmate-desktop` (Rust — the Tauri crate needs
webkit2gtk and an existing `ui/dist`, so CI type-checks it instead), `cd ui && npm test` (corpus
plus live e2e when `LEMMATE_SERVER_BIN`/`LEMMATE_CLI_BIN` point at built binaries), and
`ui/scripts/cdp.mjs` for headless-Chrome smoke runs against a running server. CI additionally
compiles the whole workspace, Tauri shell included, on macOS and Windows.

Install (build the server, CLI and desktop app on Linux, macOS or Windows):
[`docs/install.md`](docs/install.md). User guide (writing, organising, sharing, shortcuts, CLI,
export, Obsidian migration): [`docs/guide.md`](docs/guide.md).

## Accounts and access

`lemmate-server` has accounts on by default. The first account to register becomes the admin;
after that only admins create accounts unless `--allow-registration` is set. Sessions are
opaque tokens (hashed at rest), sent as `Authorization: Bearer …` by native clients and as an
HttpOnly cookie by the browser. Vaults have members with roles — **owner** (manages members),
**editor**, **viewer** — and a vault nobody owns yet is claimed by the first user who syncs it.
The relay checks every frame: viewers can read, editors write. `--no-auth` turns all of this
off for local development (the server warns loudly; never expose it that way).

On a server with registration closed, `lemmate invite` mints **single-use registration invites**
(admin only; `--list`, `--revoke`, `--expires-days`). The recipient redeems one with `lemmate
login --invite <link-or-token>`, which implies `--register`, or by opening the link in a browser.
`lemmate passwd` changes a password: your own, which asks for the current one and drops your
other sessions, or — with `--email`, as an admin — someone else's, which does not.

```sh
lemmate-server --data-dir ./data --web-dir ui/dist            # accounts on
lemmate login --server https://notes.example.org --email you@example.org --register   # first account
lemmate sync  --vault ~/vault --server https://notes.example.org   # uses the saved token
```

## Vaults in the client

The web and desktop clients show **every vault you can read at once**: the tree's roots are the
vaults, tabs may hold notes from different ones, the quick switcher lists them all, and search
runs across them (`GET /api/v1/search`). Tags, history, trash and sharing are per vault and
follow the focused note. It is all one WebSocket — frames are addressed by doc id, so the
connection was never per vault. A vault can be given a name (stored in the vault doc, shared
with every replica); without one the tree shows a short form of its id. The local relay serves
exactly one vault, so on the desktop the tree simply has a single root.

The Files sidebar offers two browsers over the same folders, switched from its toolbar: the
interleaved **tree**, and a **folders/notes split** after Obsidian's *File Tree Alternative*
(folders on top, the selected folder's notes below, with a toggle for reaching into
subfolders). The toolbar also expands and collapses everything and reveals the open note. The
sidebar itself, and the split inside it, are draggable.

Both browsers **multi-select** (Ctrl/Cmd-click, Shift-click for a range), **drag** notes and
folders onto any folder or vault row, and carry a **right-click menu**. A move inside a vault
is a rename and `[[links]]` follow it; a move to another vault is a copy plus a delete — new
id, attachments carried over — and is confirmed first.

Each **pane** shows its note in one of three views (SPEC §8), switched in the note header or
with `Ctrl+E`: **live** preview, plain **source**, or **reading** (rendered, read-only). All
three are the same CodeMirror view reconfigured, so there is no second renderer to drift.

Opening a note **reuses the current tab**, so browsing does not accumulate tabs; the displaced
one goes on the `Ctrl+Shift+T` stack. Pinned tabs are never displaced. The ＋ on the tab strip
opens an empty tab, and the right-click menu can open into a new tab or a new pane.

**Importing an Obsidian vault** is a command in the palette, or the ⇥ button on a vault row:
pick the folder and the browser uploads it in batches to `POST /api/v1/vaults/{vault}/import`,
which runs the same conversion as `lemmate import obsidian` (callouts → fenced divs, image
embeds → `![](…)`, bookmarks and daily-note settings kept). Re-uploading skips paths the vault
already has, so an interrupted import is resumed by running it again.

## Export

`POST /api/v1/vaults/{v}/notes/{id}/export {"format": "html"|"docx"|"pdf"|"revealjs"|"beamer"|"markdown"}`
renders a note through **pandoc** (`--pandoc PATH` / `LEMMATE_PANDOC`, default: on `PATH`; 501 when
absent) with the SPEC §5 reader extensions, wikilinks included; a vault `export/` folder may
provide `defaults.yaml`, `references.bib`, `style.csl`. PDF/Beamer need a LaTeX engine next to
pandoc.

## Desktop

`lemmate-desktop` reads `desktop.toml` from the per-user configuration directory
(`~/.config/lemmate`, `~/Library/Application Support/lemmate`, `%APPDATA%\lemmate`;
`LEMMATE_CONFIG_DIR` overrides); without one it opens a setup screen
(vault folder, server, optional account) and writes it. Sessions come from `lemmate login` or the
setup screen. The window is the web client served by the embedded relay, so it works offline.

## `lemmate sync`

```sh
# machine 1: publish a folder as a new vault (prints the vault id; also stored in .lemmate/)
lemmate sync --vault ~/vault --server http://127.0.0.1:8080 --once

# machine 2: join it into an empty folder, then keep watching
lemmate sync --vault ~/vault --server http://127.0.0.1:8080 --vault-id <ULID>
```

Add `--serve 127.0.0.1:8081 --web-dir ui/dist` to also run the **local relay**: the engine
serves the sync socket, the API (from the local store), and the web client on loopback, so a UI
at `http://127.0.0.1:8081/` keeps working with the server unreachable; edits are journaled and
pushed when it returns. This is what the desktop app embeds.

Without `--once` the command runs until interrupted, reconnecting with backoff; while offline,
edits are journaled in `<vault>/.lemmate/local.db` and reconciled on reconnect. Renames are
detected by content hash within 2 s; deletions go to the trash (the note's history is kept).

**Attachments.** Any local file a note references — `![[logo.png]]`, `![alt](img/x.png)`,
`[pdf](../attachments/paper.pdf)` — is an attachment: it is uploaded (content-addressed, blake3)
and recorded in the vault doc as *path → hash*, so other replicas fetch it to the same relative
place. Unreferenced files are not synced. Editing an attachment re-uploads it; deleting one
that is still referenced restores it (drop the reference to drop the file).

**TLS.** `--server https://…` uses `wss://` for sync and `https://` for transfers. A private CA
is trusted with `--ca-cert ca.pem` (`LEMMATE_CA_CERT`); the server itself expects a reverse proxy
or platform TLS in front of it.

**History.** Every update is journaled. A snapshot is taken after 500 updates or 10 minutes;
updates older than 90 days that a snapshot makes redundant are pruned (server flags
`--snapshot-every-updates`, `--snapshot-every-minutes`, `--retain-days`).

## Build and test

```sh
cargo build --workspace --exclude lemmate-desktop
cargo test  --workspace --exclude lemmate-desktop      # desktop: `cargo check -p lemmate-desktop`
cargo run -p lemmate-cli -- doctor
cargo run -p lemmate-cli -- index corpus/basic.md --json
cargo run -p lemmate-cli -- search /path/to/vault "quick fox"
cargo run -p lemmate-cli -- sync --vault /path/to/vault --server http://127.0.0.1:8080 --once
cargo run -p lemmate-server -- --data-dir ./data        # http://127.0.0.1:8080/healthz
cargo run -p lemmate-desktop -- --vault-dir /path/to/vault --server-url http://127.0.0.1:8080
(cd ui && npm install && npm test)                    # TypeScript side of the corpus test
```

Requires Rust 1.95+ and, for the web assets, Node 24+. SQLite is bundled. `pandoc`/`quarto` are
optional and only used for export. Per-platform prerequisites, `cargo install`, and the desktop
bundles (`.deb`/AppImage, `.dmg`, `.msi`) are in [`docs/install.md`](docs/install.md).

## Sync protocol in one paragraph

One WebSocket per client. Each binary message is a frame: `u16 doc-id length | doc id |
Yjs v1 protocol message`. Doc ids are note ULIDs or `vault:<ulid>`. A client sends
`SyncStep1(state vector)` per doc it wants; the server replies with `SyncStep2` (what the client
is missing) and its own `SyncStep1`; thereafter both sides exchange `Update` messages, which the
server persists and fans out to other subscribers of that doc. Awareness messages are relayed
as-is. Permission checks (M2) gate `SyncStep1` (read) and `Update` (write).

## Layout to come

`crates/mobile` — the Tauri 2 shell for Android and iOS (M3). Everything else in the table above
exists today.
