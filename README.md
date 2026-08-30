# notes

Self-hosted, open-source, multi-user markdown notes. See [SPEC.md](SPEC.md) for the full
specification; this README covers the repository and the current milestone.

## Status: M0 — foundations (in progress)

| Crate | Path | What it is |
|---|---|---|
| `notes-core` | `crates/core` | Shared engine: yrs CRDT docs (note + vault), text-diff application, SQLite update log + snapshots + FTS, on-disk projection and external-edit ingestion, filesystem watcher, sync frame codec, markdown indexer, and the **client sync engine** (`client::run`). |
| `notes-server` | `crates/server` | axum: WebSocket sync relay with persistence and snapshot/pruning policy, derives notes/tags/FTS from the CRDT stream, content-addressed attachment store, minimal REST (`/api/v1`). **No auth yet.** |
| `notes-cli` | `crates/cli` | `notes` binary: `sync`, `index`, `search`, `doctor`. |
| `notes-ui` | `ui/` | TypeScript: markdown indexer sharing the corpus with `notes-core`; later CodeMirror 6 + Svelte 5 shell. |
| corpus | `corpus/` | Markdown conformance cases (`*.md` + expected `*.json`) that both indexers must satisfy. |

Done in M0 so far: CRDT doc model with merge tests, diff-to-CRDT edits, store schema and
round-trip tests, projection write/ingest with a 3-way merge test, watcher, wire framing,
markdown indexing, server relay with an end-to-end WebSocket test, and `notes sync` — the full
projection loop (watch → debounce → ingest/create/rename/delete → WebSocket → project), with an
end-to-end test that drives two directories through a real server across create, edit,
concurrent offline edits, rename, and delete.

Also done: attachments (referenced files are hashed, uploaded, recorded in the vault doc, and
fetched by other replicas) and the snapshot/pruning policy for the update log.

Also done: `wss://`/`https://` with a private-CA option, orphan attachment cleanup (client
drops unreferenced vault entries; server purges blobs after a grace period), and the
TypeScript indexer in `ui/` that passes the same corpus (`cd ui && npm test`).

M0 is feature-complete against SPEC §16. Carry-overs into M1: write `id:` into front matter on
note creation (SPEC §6.3, decided 2026-08-30) and per-note `NoteAttachment` rows on the server.

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

`crates/desktop`, `crates/mobile` (Tauri 2 shells), `ui/` (TypeScript: CodeMirror 6 + Yjs).
