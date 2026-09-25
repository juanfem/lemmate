# Lemmate

Self-hosted, open-source, multi-user markdown notes — a replacement for Obsidian that you run
yourself and can write in together.

## What makes it different

**Your notes are plain files, and they never conflict.** A vault is always a folder of `.md`
files plus their attachments: zip it, `grep` it, open it in vim, point pandoc at it. Underneath,
every note is a CRDT (Yjs/yrs), and the file is a *projection* of it. Edits from two laptops
that were offline for a week, from a colleague typing in the same paragraph, or from vim on the
file itself all merge — there are no `conflicted copy` files and no conflict markers. Leaving
Lemmate never needs an export step.

**Real-time collaboration on your own server.** Share a whole vault or a single note with other
people as owner, editor or viewer, or publish a read-only public link. Everyone in a note sees
the others' cursors. It is one small binary with one SQLite file and no third-party service, shipped
as a Docker image that runs on a machine at home or any small rented server.

**A server is optional, and not a mode.** The desktop app runs fully standalone: search,
backlinks, tags, history, trash, daily notes, templates, attachments, import and export all
work from a local sidecar, with nothing on the network. When you want your other devices or
other people, *Connect a server…* uploads every vault — history included — and the app carries
on. Two vaults that should have been one can be **merged**, keeping note ids, history and
links.

**Offline everywhere.** The desktop app keeps a full local copy and journals edits until the
server is back. On a phone, the web client installs to the home screen, pulls the whole vault
into IndexedDB, and reads, edits and searches with the network down.

**Every vault at once.** One window shows every vault you can read as roots of one tree, with
one search across all of them, tabs and split panes mixing notes from any of them.

**Pandoc-flavoured markdown, rendered properly.** Maths, citations, fenced divs, callouts,
tables, footnotes, wikilinks and note transclusion, in a CodeMirror 6 *live preview* that never
rewrites syntax it does not understand. Export any note through **pandoc** (HTML, DOCX, PDF,
reveal.js, Beamer), or render it with **Quarto** into a pane beside the note — reveal.js decks
included, with a speaker view, and PDF through Typst with no LaTeX install.

**Scriptable instead of pluggable.** No plugin system, on purpose. Instead: a REST API, a
`lemmate` CLI that works on local folders and remote vaults alike, and an **MCP server** so AI
agents can search, read and write notes (edits are diff-merged into the CRDT like anyone else's).

**Coming from Obsidian is one command.** Import converts callouts, embeds, bookmarks and
daily-note settings — from the CLI, or from the app by picking the folder.

What it deliberately is not: no graph view, canvas or query language, no WYSIWYG, no
peer-to-peer sync, no end-to-end encryption (the server needs to read notes to search, share and
render them — encrypt the disk). [SPEC.md](SPEC.md) explains each decision.

## Getting started

Ready-made, unsigned builds for Linux, macOS and Windows — the server and CLI with the web
client, plus desktop installers (`.deb`/`.rpm`/AppImage, `.dmg`, `.msi`/NSIS) — are produced
by CI on every push to `main`, and a `v*` tag drafts a release from them. To build from source,
see [docs/install.md](docs/install.md).

**Just me, one machine.** Start `lemmate-desktop`, pick a notes folder, and leave *Sync with a
server* unticked. Or, headless in a browser:

```sh
lemmate serve --root ~/lemmate --web-dir ui/dist
```

**A server for my devices, or for a team.**

```sh
docker build -t lemmate . && docker run -p 8080:8080 -v lemmate_data:/data lemmate
# or, without Docker:
lemmate-server --data-dir ./data --web-dir ui/dist
```

The first account to register becomes the admin; after that, accounts come from admins or
single-use invites (`lemmate invite`). Put it behind a TLS reverse proxy —
[docs/deploy.md](docs/deploy.md) covers Docker with Caddy (at home or on a rented server),
backups, and every flag. Then open the server in a browser, point the desktop app at it, or
keep a plain folder in sync from the command line:

```sh
lemmate login --server https://notes.example.org --email you@example.org
lemmate sync  --vault ~/vault --server https://notes.example.org
```

The [user guide](docs/guide.md) covers writing, organising, sharing, keyboard shortcuts, the
phone layout, the CLI, export and rendering, and migrating from Obsidian.

## Status

**M0–M2 are done:** the sync core and file projection, the web and desktop clients, accounts,
roles, sharing and presence. **M3** (power features) has landed pandoc export, Quarto rendering,
REST and relay writes, the MCP server and remote CLI, the all-vaults workspace, Obsidian import
from the UI, transclusion, a file manager for non-note files, and the installable offline web
client with a phone layout and a toolbar above its keyboard, a floating format bar and
right-click menu in the editor, OIDC sign-in (with password login optional), personal access
tokens scoped to vaults and to reading, saved tokens in the OS keychain, per-vault daily-note
settings with a calendar, and per-note bibliographies at export.

There is no native mobile app, by decision: a Tauri mobile shell existed and was removed in
favour of the installed web client, which already does everything it was for (SPEC §14).

## Repository

| Crate / package | Path | What it is |
|---|---|---|
| `lemmate-core` | `crates/core` | The engine everything shares: CRDT docs for notes and vaults, the SQLite update log with snapshots and FTS, file projection and external-edit ingestion, the markdown indexer, attachments, the sync client and its **local relay**, import, export, pandoc and Quarto. |
| `lemmate-server` | `crates/server` | axum: the WebSocket sync relay with persistence and retention, derived notes/tags/search, content-addressed attachments, accounts, roles and shares, the REST API, and the web client. |
| `lemmate-cli` | `crates/cli` | The `lemmate` binary: account commands, `sync`, `serve`, remote `ls/cat/new/edit/mv/rm/daily/find/backlinks/tags`, `mcp`, `import obsidian`, `export zip`, `doctor` — see [crates/cli/README.md](crates/cli/README.md). |
| `lemmate-desktop` | `crates/desktop` | Tauri 2 shell: runs one sync engine per vault behind one local relay — with a server or without — and opens the web client on it. |
| `lemmate-ui` | `ui/` | Svelte 5 + CodeMirror 6 client, used by the browser and the desktop app alike, and the TypeScript markdown indexer. |
| corpus | `corpus/` | Markdown cases the Rust and TypeScript indexers must agree on. |

### How it fits together

The desktop app does not talk to the server directly. It embeds a **local relay** — the same
protocol and API the server speaks, answered from each vault's `.lemmate/` sidecar — and the
window is the ordinary web client pointed at it. That is why the desktop works offline, why
standalone is the full app, and why there is only one UI to maintain.

Sync is one WebSocket per client. Each binary frame is `u16 doc-id length | doc id | Yjs v1
message`, where the doc id is a note ULID or `vault:<ulid>`, so every vault shares the one
socket. A client sends `SyncStep1` per doc; the server answers with what the client is missing
and its own state vector; after that both sides exchange updates, which the server persists and
fans out. Permission checks gate reads on `SyncStep1` and writes on `Update`. Server-side
metadata — note list, tags, links, full-text index — is derived from that stream, never a second
source of truth.

## Build and test

```sh
cargo build --workspace --exclude lemmate-desktop
cargo test  --workspace --exclude lemmate-desktop
cargo check -p lemmate-desktop                 # needs webkit2gtk, and an existing ui/dist
(cd ui && npm install && npm run build && npm test)
cargo run -p lemmate-server -- --data-dir ./data --web-dir ui/dist    # http://127.0.0.1:8080
```

Requires Rust 1.95+ and Node 24+; SQLite is bundled; `pandoc` and `quarto` are optional. The
UI tests run live end-to-end suites too when `LEMMATE_SERVER_BIN`/`LEMMATE_CLI_BIN` point at
built binaries, and `ui/scripts/cdp.mjs` drives headless Chrome against a running server. CI
also compiles the whole workspace, the Tauri shell included, on macOS and Windows.

## License

MIT — see [LICENSE](LICENSE). Dependencies keep their own licences.
