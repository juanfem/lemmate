# notes

Self-hosted, open-source, multi-user markdown notes. See [SPEC.md](SPEC.md) for the full
specification; this README covers the repository and the current milestone.

## Status: M0 — foundations (in progress)

| Crate | Path | What it is |
|---|---|---|
| `notes-core` | `crates/core` | Shared engine: yrs CRDT docs (note + vault), text-diff application, SQLite update log + snapshots + FTS, on-disk projection and external-edit ingestion, filesystem watcher, sync frame codec, markdown indexer, and the **client sync engine** (`client::run`). |
| `notes-server` | `crates/server` | axum: WebSocket sync relay with persistence, derives notes/tags/FTS from the CRDT stream, minimal REST (`/api/v1`). **No auth yet.** |
| `notes-cli` | `crates/cli` | `notes` binary: `sync`, `index`, `search`, `doctor`. |
| corpus | `corpus/` | Markdown conformance cases (`*.md` + expected `*.json`) that the Rust indexer and the future JS parser must both satisfy. |

Done in M0 so far: CRDT doc model with merge tests, diff-to-CRDT edits, store schema and
round-trip tests, projection write/ingest with a 3-way merge test, watcher, wire framing,
markdown indexing, server relay with an end-to-end WebSocket test, and `notes sync` — the full
projection loop (watch → debounce → ingest/create/rename/delete → WebSocket → project), with an
end-to-end test that drives two directories through a real server across create, edit,
concurrent offline edits, rename, and delete.

Still to do for M0: attachment upload, snapshot/pruning policy, TLS for `wss://`, and the JS
side of the parser conformance test.

## `notes sync`

```sh
# machine 1: publish a folder as a new vault (prints the vault id; also stored in .notes/)
notes sync --vault ~/vault --server http://127.0.0.1:8080 --once

# machine 2: join it into an empty folder, then keep watching
notes sync --vault ~/vault --server http://127.0.0.1:8080 --vault-id <ULID>
```

Without `--once` the command runs until interrupted, reconnecting with backoff; while offline,
edits are journaled in `<vault>/.notes/local.db` and reconciled on reconnect. Renames are
detected by content hash within 2 s; deletions go to the trash (the note's history is kept).

## Build and test

```sh
cargo build
cargo test
cargo run -p notes-cli -- doctor
cargo run -p notes-cli -- index corpus/basic.md --json
cargo run -p notes-cli -- search /path/to/vault "quick fox"
cargo run -p notes-cli -- sync --vault /path/to/vault --server http://127.0.0.1:8080 --once
cargo run -p notes-server -- --data-dir ./data        # http://127.0.0.1:8080/healthz
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
