# lemmate-cli

The `lemmate` binary: local commands over a vault directory, server-backed commands over the REST
API, and an MCP server for agents. See [SPEC.md](../../SPEC.md) §13 for the contract.

## CLI and MCP

Local commands work on a directory (`index`, `search`, `import obsidian`, `export zip`,
`doctor`) or keep one in sync with a server (`sync`); they are documented in the
[repository README](../../README.md).

The commands below talk to a server over `/api/v1` instead, so nothing is projected to disk.
They all take the same connection options:

| Option | Environment | Meaning |
|---|---|---|
| `--server URL` | `LEMMATE_SERVER` | Server base URL, e.g. `https://notes.example.org`. Required. |
| `--token TOKEN` | `LEMMATE_TOKEN` | Access token. Defaults to the one `lemmate login` saved for this server (`credentials.toml` in the per-user configuration directory: `~/.config/lemmate` on Linux, `~/Library/Application Support/lemmate` on macOS, `%APPDATA%\lemmate` on Windows; `LEMMATE_CONFIG_DIR` overrides it). |
| `--ca-cert FILE` | `LEMMATE_CA_CERT` | PEM of a private CA to trust for `https://`. |
| `--vault ULID` | `LEMMATE_VAULT` | Which vault to work in. Optional when the account has exactly one. |

A note is named either by its vault-relative path — `Projects/plan.md`, or just
`Projects/plan`, or even `plan` when the file name is unambiguous — or by its ULID.

| Command | What it does |
|---|---|
| `lemmate vaults [--json]` | List the vaults this account can see, with note counts. |
| `lemmate ls [--json]` | List the vault's notes, one path per line. |
| `lemmate cat <note> [--json]` | Print a note's markdown (`--json` gives id, path, title, content). |
| `lemmate new <path> [--from FILE]` | Create a note. Content comes from `--from`, from stdin when it is piped (`--from -` forces this), else the note starts empty. |
| `lemmate edit <note> [--from FILE]` | Fetch the note, open `$VISUAL`/`$EDITOR` on it, send the result back. `--from` skips the editor. |
| `lemmate mv <note> <new-path>` | Move or rename. |
| `lemmate rm <note>` | Move to the trash (history is kept). |
| `lemmate daily [YYYY-MM-DD] [--json]` | Print the daily note for a date (today by default), creating `Daily/<date>.md` if needed. |
| `lemmate find <query> [--limit N] [--json]` | Full-text search the vault on the server. (The local `lemmate search <dir> <query>` walks a directory instead.) |
| `lemmate backlinks <note> [--json]` | The notes that link to this one. |
| `lemmate tags [--json]` | The vault's tags, most used first. |
| `lemmate mcp` | Serve the Model Context Protocol on stdin/stdout. |

Writes are never blind overwrites: the server diffs the text you send against the current
content and applies the difference as CRDT edits, so they merge with whoever else is editing.

```sh
lemmate login --server https://notes.example.org --email you@example.org
export LEMMATE_SERVER=https://notes.example.org
lemmate vaults
lemmate new Meetings/standup < draft.md
lemmate ls
lemmate cat Meetings/standup
lemmate find "sync protocol" --json | jq -r '.[].note_id'
lemmate daily >> today.md
```

Every command exits non-zero with a one-line reason on failure: "not signed in: run `notes
login`" for a 401, "not found or no access" for a 404.

### MCP server

`lemmate mcp` speaks [Model Context Protocol](https://modelcontextprotocol.io) JSON-RPC 2.0 over
stdio (one message per line; stdout carries protocol traffic only, logs go to stderr). It
negotiates `2024-11-05`, `2025-03-26`, or `2025-06-18`, and offers:

- **Tools** — `search_notes`, `read_note`, `write_note` (diff-merged, creates missing paths),
  `append_to_note`, `list_notes`, `get_daily_note` (get-or-create), `get_backlinks`,
  `list_tags`.
- **Resources** — one `note://<vault>/<path>` entry per note, `text/markdown`.

Point a client at it with:

```json
{
  "command": "lemmate",
  "args": ["mcp", "--server", "https://notes.example.org", "--vault", "01J…"],
  "env": { "LEMMATE_TOKEN": "…" }
}
```

`--vault` may be left out when the account has exactly one vault, and `LEMMATE_TOKEN` when
`lemmate login` has already saved a token for that server.

## Layout

| File | What it is |
|---|---|
| `src/main.rs` | The `lemmate` binary: clap definitions and command handlers. |
| `src/remote.rs` | Blocking REST client (`Remote`) and the `NotesApi` trait the commands and MCP tools are written against. |
| `src/mcp.rs` | The MCP server: a dispatcher over `serde_json::Value` plus the stdio loop. |
| `tests/sync_e2e.rs` | Two vault directories synced through a real server. |
| `tests/remote_e2e.rs` | `Remote` and the MCP dispatcher against a real server, in-process. |
