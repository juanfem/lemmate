# notes

Self-hosted, open-source, multi-user markdown notes. See [SPEC.md](SPEC.md) for the full
specification; this README covers the repository and the current milestone.

## Status: M1 — single-user desktop MVP (in progress)

| Crate / package | Path | What it is |
|---|---|---|
| `notes-core` | `crates/core` | Shared engine: yrs CRDT docs (note + vault), text-diff application, SQLite update log + snapshots + FTS, on-disk projection and external-edit ingestion, watcher, markdown indexer, attachments, TLS, the client sync engine (`client::run`) and its **local relay** (`client::start`), Obsidian import, zip export. |
| `notes-server` | `crates/server` | axum: WebSocket sync relay with persistence and retention policy, derived notes/tags/FTS, content-addressed attachments with orphan purge, accounts/sessions/vault roles enforced on REST and the relay, REST (`/api/v1`), serves the web client. |
| `notes-cli` | `crates/cli` | `notes` binary: `login`/`logout`, `sync` (with `--serve` relay), `index`, `search`, `import obsidian`, `export zip`, `doctor`. |
| `notes-desktop` | `crates/desktop` | Tauri 2 shell: starts the relay for the configured vault and opens one window on it. |
| `notes-ui` | `ui/` | Svelte 5 + CodeMirror 6 client: live preview (headings, emphasis, code, links, wikilinks/embeds, math, tags, tasks, quotes, callouts, tables, folded front matter), `[[`/`#` autocomplete, tree, tabs, quick switcher, command palette, search, tags, outline, backlinks, bookmarks, history, daily notes + templates, paste/drop attachments, sharing (users, public links), presence, login. Also the markdown indexer sharing `corpus/` with `notes-core`. |
| corpus | `corpus/` | Markdown conformance cases both indexers must satisfy. |

M0 and M1 are complete except split panes and a first-run setup screen for the desktop app
(today it reads `~/.config/notes/desktop.toml`). M2: accounts and vault roles, per-note shares
and public read-only links, version history, and presence are done; see `docs/deploy.md` for
Docker and fly.io.

Verification: `cargo test --workspace` (Rust), `cd ui && npm test` (corpus + live e2e when
`NOTES_SERVER_BIN`/`NOTES_CLI_BIN` point at built binaries), and `ui/scripts/cdp.mjs` for
headless-Chrome smoke runs against a running server.

## Accounts and access

`notes-server` has accounts on by default. The first account to register becomes the admin;
after that only admins create accounts unless `--allow-registration` is set. Sessions are
opaque tokens (hashed at rest), sent as `Authorization: Bearer …` by native clients and as an
HttpOnly cookie by the browser. Vaults have members with roles — **owner** (manages members),
**editor**, **viewer** — and a vault nobody owns yet is claimed by the first user who syncs it.
The relay checks every frame: viewers can read, editors write. `--no-auth` turns all of this
off for local development (the server warns loudly; never expose it that way).

```sh
notes-server --data-dir ./data --web-dir ui/dist            # accounts on
notes login --server https://notes.example.org --email you@example.org --register   # first account
notes sync  --vault ~/vault --server https://notes.example.org   # uses the saved token
```

## `notes sync`

```sh
# machine 1: publish a folder as a new vault (prints the vault id; also stored in .notes/)
notes sync --vault ~/vault --server http://127.0.0.1:8080 --once

# machine 2: join it into an empty folder, then keep watching
notes sync --vault ~/vault --server http://127.0.0.1:8080 --vault-id <ULID>
```

Add `--serve 127.0.0.1:8081 --web-dir ui/dist` to also run the **local relay**: the engine
serves the sync socket, the API (from the local store), and the web client on loopback, so a UI
at `http://127.0.0.1:8081/` keeps working with the server unreachable; edits are journaled and
pushed when it returns. This is what the desktop app embeds.

Without `--once` the command runs until interrupted, reconnecting with backoff; while offline,
edits are journaled in `<vault>/.notes/local.db` and reconciled on reconnect. Renames are
detected by content hash within 2 s; deletions go to the trash (the note's history is kept).

**Attachments.** Any local file a note references — `![[logo.png]]`, `![alt](img/x.png)`,
`[pdf](../attachments/paper.pdf)` — is an attachment: it is uploaded (content-addressed, blake3)
and recorded in the vault doc as *path → hash*, so other replicas fetch it to the same relative
place. Unreferenced files are not synced. Editing an attachment re-uploads it; deleting one
that is still referenced restores it (drop the reference to drop the file).

**TLS.** `--server https://…` uses `wss://` for sync and `https://` for transfers. A private CA
is trusted with `--ca-cert ca.pem` (`NOTES_CA_CERT`); the server itself expects a reverse proxy
or platform TLS in front of it.

**History.** Every update is journaled. A snapshot is taken after 500 updates or 10 minutes;
updates older than 90 days that a snapshot makes redundant are pruned (server flags
`--snapshot-every-updates`, `--snapshot-every-minutes`, `--retain-days`).

## Build and test

```sh
cargo build
cargo test
cargo run -p notes-cli -- doctor
cargo run -p notes-cli -- index corpus/basic.md --json
cargo run -p notes-cli -- search /path/to/vault "quick fox"
cargo run -p notes-cli -- sync --vault /path/to/vault --server http://127.0.0.1:8080 --once
cargo run -p notes-server -- --data-dir ./data        # http://127.0.0.1:8080/healthz
cargo run -p notes-desktop -- --vault-dir /path/to/vault --server-url http://127.0.0.1:8080
(cd ui && npm install && npm test)                    # TypeScript side of the corpus test
```

Requires Rust 1.95+. SQLite is bundled. `pandoc`/`quarto` are optional and only used for export.

## Sync protocol in one paragraph

One WebSocket per client. Each binary message is a frame: `u16 doc-id length | doc id |
Yjs v1 protocol message`. Doc ids are note ULIDs or `vault:<ulid>`. A client sends
`SyncStep1(state vector)` per doc it wants; the server replies with `SyncStep2` (what the client
is missing) and its own `SyncStep1`; thereafter both sides exchange `Update` messages, which the
server persists and fans out to other subscribers of that doc. Awareness messages are relayed
as-is. Permission checks (M2) gate `SyncStep1` (read) and `Update` (write).

## Layout to come

`crates/mobile` (Tauri 2 shell for Android/iOS), and the CodeMirror 6 + Svelte 5 app in `ui/`.
