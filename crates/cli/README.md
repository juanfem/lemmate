# notes-cli

The `notes` binary: local commands over a vault directory, server-backed commands over the REST
API, and an MCP server for agents. See [SPEC.md](../../SPEC.md) §13 for the contract.

## CLI and MCP

Local commands work on a directory (`index`, `search`, `import obsidian`, `export zip`,
`doctor`) or keep one in sync with a server (`sync`); they are documented in the
[repository README](../../README.md).

The commands below talk to a server over `/api/v1` instead, so nothing is projected to disk.
They all take the same connection options:

| Option | Environment | Meaning |
|---|---|---|
| `--server URL` | `NOTES_SERVER` | Server base URL, e.g. `https://notes.example.org`. Required. |
| `--token TOKEN` | `NOTES_TOKEN` | Access token. Defaults to the one `notes login` saved for this server (`~/.config/notes/credentials.toml`). |
| `--ca-cert FILE` | `NOTES_CA_CERT` | PEM of a private CA to trust for `https://`. |
| `--vault ULID` | `NOTES_VAULT` | Which vault to work in. Optional when the account has exactly one. |

A note is named either by its vault-relative path — `Projects/plan.md`, or just
`Projects/plan`, or even `plan` when the file name is unambiguous — or by its ULID.

| Command | What it does |
|---|---|
| `notes vaults [--json]` | List the vaults this account can see, with note counts. |
| `notes ls [--json]` | List the vault's notes, one path per line. |
| `notes cat <note> [--json]` | Print a note's markdown (`--json` gives id, path, title, content). |
| `notes new <path> [--from FILE]` | Create a note. Content comes from `--from`, from stdin when it is piped (`--from -` forces this), else the note starts empty. |
| `notes edit <note> [--from FILE]` | Fetch the note, open `$VISUAL`/`$EDITOR` on it, send the result back. `--from` skips the editor. |
| `notes mv <note> <new-path>` | Move or rename. |
| `notes rm <note>` | Move to the trash (history is kept). |
| `notes daily [YYYY-MM-DD] [--json]` | Print the daily note for a date (today by default), creating `Daily/<date>.md` if needed. |
| `notes find <query> [--limit N] [--json]` | Full-text search the vault on the server. (The local `notes search <dir> <query>` walks a directory instead.) |
| `notes backlinks <note> [--json]` | The notes that link to this one. |
| `notes tags [--json]` | The vault's tags, most used first. |
| `notes mcp` | Serve the Model Context Protocol on stdin/stdout. |

Writes are never blind overwrites: the server diffs the text you send against the current
content and applies the difference as CRDT edits, so they merge with whoever else is editing.

```sh
notes login --server https://notes.example.org --email you@example.org
export NOTES_SERVER=https://notes.example.org
notes vaults
notes new Meetings/standup < draft.md
notes ls
notes cat Meetings/standup
notes find "sync protocol" --json | jq -r '.[].note_id'
notes daily >> today.md
```

Every command exits non-zero with a one-line reason on failure: "not signed in: run `notes
login`" for a 401, "not found or no access" for a 404.

### MCP server

`notes mcp` speaks [Model Context Protocol](https://modelcontextprotocol.io) JSON-RPC 2.0 over
stdio (one message per line; stdout carries protocol traffic only, logs go to stderr). It
negotiates `2024-11-05`, `2025-03-26`, or `2025-06-18`, and offers:

- **Tools** — `search_notes`, `read_note`, `write_note` (diff-merged, creates missing paths),
  `append_to_note`, `list_notes`, `get_daily_note` (get-or-create), `get_backlinks`,
  `list_tags`.
- **Resources** — one `note://<vault>/<path>` entry per note, `text/markdown`.

Point a client at it with:

```json
{
  "command": "notes",
  "args": ["mcp", "--server", "https://notes.example.org", "--vault", "01J…"],
  "env": { "NOTES_TOKEN": "…" }
}
```

`--vault` may be left out when the account has exactly one vault, and `NOTES_TOKEN` when
`notes login` has already saved a token for that server.

## Layout

| File | What it is |
|---|---|
| `src/main.rs` | The `notes` binary: clap definitions and command handlers. |
| `src/remote.rs` | Blocking REST client (`Remote`) and the `NotesApi` trait the commands and MCP tools are written against. |
| `src/mcp.rs` | The MCP server: a dispatcher over `serde_json::Value` plus the stdio loop. |
| `tests/sync_e2e.rs` | Two vault directories synced through a real server. |
| `tests/remote_e2e.rs` | `Remote` and the MCP dispatcher against a real server, in-process. |
