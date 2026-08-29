# Notes — Specification

Status: draft v0.1 (2026-08-29)
Decisions marked **[decided]** are settled; **[recommended]** are proposals awaiting confirmation; **[open]** need an answer.

---

## 1. Purpose

A self-hosted, open-source, multi-user markdown note-taking app to replace Obsidian for a
focused set of workflows: writing pandoc-flavoured markdown with maths and figures,
hierarchical organisation with tags and links, daily notes, search, offline use on laptop
and phone, and real-time collaboration with other people on selected notes or whole vaults.

Deliberately *not* a platform: no plugin system, no graph view, no canvas, no query language.
Extensibility is provided by an HTTP API, a CLI, and an MCP server instead.

### 1.1 Principles

1. **Your notes are files.** A vault is always materialised as a plain folder of `.md` files
   plus attachments. You can zip it, `grep` it, open it in vim, or point pandoc at it at any
   time. Leaving the app never requires an export step.
2. **The CRDT is the truth; the file is a view.** Concurrent edits — offline, multi-device,
   multi-user — never produce conflict markers. External edits to the files are absorbed as
   ordinary edits.
3. **Lossless editing.** The editor never rewrites syntax it does not understand.
4. **Self-hosted first.** One binary, one SQLite file, one attachments directory. No mandatory
   third-party services. A fly.io recipe exists for people without a home server.
5. **Keyboard first.** Every navigation action has a shortcut; the command palette reaches
   every command.
6. **Small surface.** A feature is added only if it removes friction from the workflows in §1.

---

## 2. Glossary

| Term | Meaning |
|---|---|
| Vault | A tree of notes and attachments owned by a user and optionally shared. Maps 1:1 to a folder on disk. |
| Note | A markdown document. Has a stable ID (ULID) and a mutable path inside the vault. |
| Attachment | A binary file (image, PDF, …) referenced from notes. Content-addressed. |
| Doc | The Yjs/yrs CRDT document backing one note's text. |
| Vault doc | The CRDT document holding the vault's structure (note IDs ↔ paths, folders, bookmarks). |
| Projection | The on-disk `.md` file materialised from a doc. |
| Server | The always-on sync + persistence + web + API process. |
| Client | Desktop app, mobile app, web app, or CLI. |
| Member | A user with a role on a vault. |

---

## 3. Architecture

```
            ┌─────────────────────────────────────────────────────┐
            │  server (Rust, axum)                                │
            │   sync relay (WebSocket, Yjs protocol)              │
            │   persistence (SQLite: updates, snapshots, meta)    │
            │   attachments (content-addressed dir)               │
            │   search index (SQLite FTS5)                        │
            │   auth (sessions, OIDC), REST API, MCP endpoint     │
            │   static web client                                 │
            │   pandoc / quarto export workers                    │
            └───────────▲─────────────────▲───────────────▲───────┘
                        │ ws+https        │               │
        ┌───────────────┴──────┐  ┌───────┴───────┐  ┌────┴──────┐
        │ desktop (Tauri 2)    │  │ mobile        │  │ web       │
        │  core crate (Rust)   │  │ (Tauri 2)     │  │ (browser) │
        │  local SQLite        │  │ core crate    │  │ IndexedDB │
        │  file projection     │  │ local SQLite  │  │ cache     │
        │  file watcher        │  │ file proj.*   │  │           │
        │  local FTS           │  │ local FTS     │  │           │
        └──────────────────────┘  └───────────────┘  └───────────┘
                     shared TypeScript UI (CM6 + Yjs) in all three
```

### 3.1 Components

- **`core` crate (Rust)** — shared by server, desktop, and mobile. Contains: yrs document
  handling, SQLite persistence of updates/snapshots, sync protocol client/server halves,
  file projection + external-change ingestion, markdown parsing for link/tag extraction,
  FTS indexing. No UI. This is the single most important module; the server and the native
  apps are thin shells around it.
- **`server`** — axum HTTP/WebSocket server embedding `core`. Serves the web client. Single
  static binary. Configuration by environment variables / one TOML file.
- **`ui`** — TypeScript, framework-agnostic core around CodeMirror 6 + Yjs; app shell in
  **Svelte 5 [recommended]** (small bundle, matters on mobile webviews). Identical bundle in
  desktop, mobile, and web.
- **`desktop`, `mobile`** — Tauri 2 shells. Expose `core` to the UI via Tauri commands.
- **`cli`** — `notes` binary. Talks to a server over the REST API, or to a local vault
  directly via `core` **[recommended: server-only in v1, direct-local later]**.

### 3.2 Topology **[decided]**

- Client ↔ server only. **No peer-to-peer.** Every device syncs through the server it is
  logged into. A vault lives on exactly one server.
- Native clients (desktop, mobile) are **offline-first**: full local copy, full local search,
  edits queue and merge on reconnect.
- The web client is **online-first** with a cache: recently opened notes are editable
  offline (Yjs + IndexedDB), the rest require connectivity.

### 3.3 Technology choices

| Concern | Choice | Notes |
|---|---|---|
| CRDT | Yjs (`yjs` in TS, `yrs` in Rust) **[decided: CRDT truth]** | Mature CM6 binding (`y-codemirror.next`), awareness protocol for cursors, identical wire format across languages. |
| Editor | CodeMirror 6, live-preview decorations **[decided]** | Source is always the document; decorations render previews in place. |
| Server | Rust, axum, tokio | Single binary, shares `core`. |
| Database | SQLite (WAL) | Per server, not per vault. Attachments outside the DB. |
| Search | SQLite FTS5, trigram + unicode61 | Same engine on server and native clients. |
| Native shells | Tauri 2 **[decided: native mobile]** | Linux/macOS/Windows/Android/iOS from one codebase. |
| Markdown parser (JS) | micromark + mdast with custom extensions | Used by editor decorations, reading mode, link/tag extraction on the client. |
| Markdown parser (Rust) | `markdown-rs` (micromark port) + custom extensions | Used by `core` for indexing. Must agree with the JS parser on the §5 subset — enforced by a shared conformance test corpus. |
| Maths | KaTeX | Client-side render of `$…$` / `$$…$$`. |
| Export | pandoc ≥ 3.1, quarto (optional) | Server-side or local if installed. |

---

## 4. Data model

### 4.1 Entities

```
User        id, email, display_name, password_hash?, oidc_subject?, created_at
Session     id, user_id, token_hash, expires_at, device_name
Vault       id, name, owner_id, created_at, settings (json)
Membership  vault_id, user_id, role ∈ {owner, editor, viewer}
Note        id (ULID), vault_id, path, title, created_at, updated_at, deleted_at?
NoteShare   note_id, user_id | link_token, role ∈ {editor, viewer}, expires_at?
Attachment  hash (blake3), vault_id, size, mime, filename_hint, created_at
NoteAttachment note_id, hash             -- reference tracking for orphan cleanup
DocUpdate   doc_id, seq, bytes, author_id, created_at   -- append-only Yjs updates
DocSnapshot doc_id, seq, bytes, created_at              -- compaction points
Version     note_id, seq, label?, author_id, created_at -- user-visible history points
Bookmark    (lives in vault doc, see 4.3)
```

`doc_id` is either a note ID or `vault:<vault_id>` for the vault doc.

### 4.2 Note doc

One `Y.Doc` per note:

- `content: Y.Text` — the entire markdown source, front matter included.
- Nothing else. Metadata that must be queryable (title, tags, links) is *derived* from
  content by the parser and stored in relational tables; it is never a second source of truth.

Rationale: a single `Y.Text` keeps collaboration a pure text-CRDT problem, makes CM6
binding trivial, and guarantees the projection is byte-exact.

### 4.3 Vault doc

One `Y.Doc` per vault, holding structure that must survive concurrent offline edits:

- `notes: Y.Map<noteId, { path: string, createdAt }>` — rename/move = update `path`.
- `folders: Y.Map<path, {}>` — explicit empty folders.
- `bookmarks: Y.Array<{ kind: 'note'|'folder'|'search'|'heading', target, label }>`.

Two concurrent renames of the same note resolve by Yjs last-writer-wins on the map entry;
two notes concurrently moved to the same path get a ` (2)` suffix applied by the client
that observes the collision.

### 4.4 Identity and paths

- Notes are identified by ULID, **not** path. Path is an attribute.
- Links in markdown resolve by path/name (see §5.4), but rename/move rewrites links in
  all referring notes as ordinary CRDT edits — done by the client performing the rename.
- Attachments are identified by blake3 hash. Filenames are hints only.

---

## 5. Markdown dialect

**[decided: pandoc-flavoured markdown, explicitly enumerated subset.]** The app implements
these extensions in its own parsers; anything outside the subset is preserved verbatim and
rendered as plain text. Real pandoc is used only on the export path.

### 5.1 Supported pandoc extensions

CommonMark base, plus (pandoc extension names):

| Extension | Syntax | Editor support |
|---|---|---|
| `yaml_metadata_block` | `---` front matter | Folded, property editor |
| `tex_math_dollars` | `$x$`, `$$…$$` | KaTeX preview |
| `raw_tex` | `\begin{align}…` | Passed through; KaTeX where possible |
| `pipe_tables` | `\| a \| b \|` | Rendered table, cell editing |
| `footnotes` | `[^1]`, `[^1]: …` | Hover preview |
| `fenced_divs` | `::: {.class}` | Callouts (§5.3) |
| `bracketed_spans` | `[text]{.class}` | Styled span |
| `header_attributes`, `link_attributes`, `fenced_code_attributes` | `{#id .cls key=val}` | Attribute preserved, used for anchors and image sizing |
| `implicit_figures` | `![caption](img.png)` | Inline image |
| `citations` | `[@key]`, `@key` | Autocomplete from vault `.bib`, hover preview |
| `definition_lists` | `term\n: definition` | Rendered |
| `task_lists` | `- [ ]` | Clickable checkbox |
| `strikeout` | `~~text~~` | Rendered |
| `superscript`, `subscript` | `^2^`, `~2~` | Rendered |
| `wikilinks_title_after_pipe` | `[[note\|title]]` | §5.4 |
| `backtick_code_blocks` + info strings | ```` ```{python} ```` | Syntax highlight; braces tolerated (Quarto) |

### 5.2 Front matter

YAML. Fields the app interprets; all others pass through untouched:

```yaml
title: "…"        # overrides filename as display title
tags: [a, b/c]    # merged with inline #tags
aliases: [x, y]   # alternative link targets
created: 2026-08-29
date: 2026-08-29  # Quarto-compatible
id: 01J…          # optional; written only on export/import, see §6.3
```

### 5.3 Callouts

Quarto callout syntax, so `.qmd` files render identically:

```markdown
::: {.callout-note title="Heads up"}
Body
:::
```

Kinds: `note`, `tip`, `warning`, `caution`, `important`. Collapsible via `collapse="true"`.
Obsidian `> [!note]` blockquote callouts are converted on import (§11).

### 5.4 Links

- **Wikilinks** `[[target]]`, `[[target|label]]`, `[[target#Heading]]` are the primary link
  form. Target resolution: exact path → unique basename → alias. Ambiguous basenames must
  be qualified by path.
- Standard links `[label](relative/path.md)` are supported and resolved relative to the note.
- **Embeds** `![[note]]`, `![[note#Heading]]` transclude read-only **[tier 3]**.
- Export: pandoc's `wikilinks_title_after_pipe` extension handles wikilinks natively;
  `#Heading` fragments are rewritten to pandoc auto-identifiers by an export filter.

### 5.5 Tags

`#tag` and `#nested/tag` inline (not inside code/maths), plus `tags:` front matter. Tags are
case-insensitive and hierarchical. Rename tag = rewrite all occurrences.

### 5.6 Quarto `.qmd`

`.qmd` files are first-class notes: same editor, same links, same search. The app does
**not** execute code cells. "Render with Quarto" is an export action that shells out to a
`quarto` binary on the server or desktop when present.

---

## 6. Storage

### 6.1 Server

- `notes.db` (SQLite, WAL): all tables in §4.1. Doc updates are appended per doc; a
  snapshot is written every 500 updates or 10 minutes, after which older updates may be
  pruned to the last snapshot older than the version-history retention window (§9).
- `attachments/<vault_id>/<hash[0..2]>/<hash>` on the filesystem.
- `exports/` scratch for pandoc output, TTL 1 hour.
- Backup = `sqlite3 .backup` + `rsync attachments/`. Documented as a one-liner.

### 6.2 Native clients

Per vault, a local directory chosen by the user (the projection, §6.3) plus a sidecar:

```
<vault>/
  .notes/
    local.db          # same schema subset as the server: docs, updates, snapshots, index
    attachments/      # content-addressed cache; pinned or LRU (mobile)
  Daily/2026-08-29.md
  Projects/…
  attachments/…      # human-readable projection of referenced attachments
```

### 6.3 File projection **[decided: files are a view]**

Write direction (CRDT → disk):
- Debounced 500 ms after the last local or remote change to a doc, `core` writes the full
  text atomically (write temp, rename). Path from the vault doc.
- Attachments referenced by a note are materialised under `attachments/` with
  `<filename_hint>` (deduplicated with `-<hash[0..6]>` on collision).

Read direction (disk → CRDT):
- A watcher (`notify` crate) observes the vault directory, ignoring `.notes/`.
- On change to a known file: compute a diff between the last projected text (stored in
  `local.db`) and the new file content, apply the diff as a Y.Text delta. Because the diff
  is against the *last projected* text, it composes correctly with concurrent CRDT edits.
- New file: create note with new ULID at that path.
- Deleted file: soft-delete note (trash, §9); a delete is never propagated destructively
  from disk without a trash stop.
- Moved file: if a new file's content hash equals a just-deleted file's hash within 2 s,
  treat as rename; otherwise delete + create.
- The optional `id:` front-matter field, when present, takes precedence for identity —
  this is how external tooling can move files safely.

Mobile projection: Android exposes the vault folder via the Storage Access Framework;
iOS exposes it in the Files app under the app's Documents (`UIFileSharingEnabled`).
Background watching is not reliable on mobile; the projection is reconciled on app
foreground instead.

### 6.4 Web client

`y-indexeddb` caches docs the user has opened; the vault doc is always cached. No file
projection. Attachments are fetched on demand.

---

## 7. Sync protocol

- One WebSocket per client per server, multiplexing all subscribed docs:
  `{ doc_id, kind: sync1 | sync2 | update | awareness | ack, payload }`.
- Uses the standard Yjs sync protocol (state vectors → diff → updates) per doc, and the
  Yjs awareness protocol for cursors/presence.
- Subscription = permission check. The server refuses `sync1` on a doc the session's user
  cannot read, and drops `update` on a doc they cannot edit (viewer).
- Server persists every accepted update (with `author_id`) before broadcasting.
- Reconnect: client sends state vectors for all cached docs; server replies with missing
  updates. Offline queues are just the local update log not yet acked.
- Attachments: `PUT /v/:vault/attachments/:hash` (idempotent), `GET …/:hash`. Clients
  upload before inserting the reference so a synced note never dangles. Mobile pins
  attachments of notes opened in the last 30 days, LRU beyond a configurable cache size.
- Vault doc subscriptions are automatic for members; per-note shares subscribe to the
  note doc only and see the note in a "Shared with me" view.

---

## 8. Editor

CodeMirror 6, bound to `Y.Text` via `y-codemirror.next`. Modes: **live preview** (default),
**source** (no decorations), **reading** (rendered mdast, read-only).

Live-preview decorations (hidden markup, rendered widget, revealed when the cursor enters
the range): headings, emphasis, inline code, links/wikilinks, images, maths, tables,
callouts, footnote refs, checkboxes, tags, citations, front matter (folded to a property
panel), horizontal rules, code blocks with language highlighting.

Editing features:
- Autocomplete: `[[` notes and headings, `#` tags, `@` citations, `:::` callout kinds,
  `/` slash menu for blocks (table, callout, maths, image, date).
- Paste/drop images and files → attachment upload + `![](…)` insertion. Paste URL over
  selection → link.
- Table editing: Tab/Shift-Tab across cells, row/column commands, column alignment.
- Heading folding, outline pane, jump-to-heading.
- Find/replace in note; multi-cursor; optional Vim keymap.
- Spellcheck via the platform webview.
- Collaboration: remote cursors and selections with name labels; presence list per note.
- Mobile: toolbar row above the keyboard for markup, indent, checkbox, link, image.

---

## 9. Organisation and workflow features

- **Tree** — folders are real folders. Drag-and-drop move, create, rename, delete.
  Sort by name/modified. Optional folder note (`<folder>/<folder>.md`).
- **Tabs and panes** — multiple open notes, split horizontally/vertically, pinned tabs,
  reopen closed tab. Tab state persisted per device.
- **Quick switcher** — fuzzy search over paths, titles, aliases; creates note on Enter if
  no match.
- **Command palette** — every command, with shortcut hints; per-user shortcut remapping.
- **Links & backlinks** — backlinks pane with context snippets; unlinked mentions;
  outgoing links pane.
- **Tags** — tag pane with counts, hierarchical; click = search.
- **Bookmarks** — notes, folders, headings, saved searches; ordered, in the vault doc.
- **Daily notes** — `Daily/YYYY-MM-DD.md` (path and format configurable per vault),
  template applied on creation, previous/next-day navigation, "today" shortcut, calendar
  popover.
- **Templates** — a `Templates/` folder; variables `{{date}}`, `{{date:FORMAT}}`, `{{time}}`,
  `{{title}}`, `{{cursor}}`. No scripting.
- **Version history** — a `Version` row is created every 15 minutes of activity per note
  and on explicit "save version"; history pane shows versions with author, diff view,
  restore (restore = new edit, history preserved). Raw update log retained 90 days
  (configurable), versions retained forever.
- **Trash** — deleted notes are hidden, restorable for 30 days, then purged along with
  orphaned attachments.

---

## 10. Search

- SQLite FTS5 over note content, title, tags; trigram tokenizer for substring and
  CJK, unicode61 for word queries. Same index on the server (for web and API) and in
  `local.db` (native clients).
- Query syntax: free text, `"phrase"`, `tag:x`, `path:Projects/`, `has:math`, `has:tasks`,
  `created:>2026-01-01`, `-exclude`, `OR`.
- Results with highlighted snippets, grouped by note; Enter opens at the match.
- Saved searches as bookmarks.

---

## 11. Multi-user

### 11.1 Accounts and auth

- Email + password (argon2id) and/or OIDC (any provider; Authelia/Keycloak/Google tested).
  Server config can disable password login or registration.
- Sessions are opaque tokens; native clients store them in the OS keychain.
- Personal access tokens for the CLI, API, and MCP, scoped to vaults.

### 11.2 Permissions

| Scope | Roles | Notes |
|---|---|---|
| Vault | owner, editor, viewer | Owner may invite/remove members, delete vault, change settings. |
| Note | editor, viewer | Grants access to a single note (and its attachments) to a user or via link. Overrides vault role upward only. |
| Public link | viewer | Token URL, optional expiry, rendered by the server in reading mode without login. |

Enforcement is at the sync layer (§7) and API layer, not in the UI.

### 11.3 Collaboration

- Real-time editing with remote cursors on any doc a user can edit.
- Presence: who is in the vault / in this note.
- Every update carries `author_id`; version history and a "blame" gutter surface it.

### 11.4 Obsidian import

`notes import obsidian <dir>` (CLI and desktop wizard):
- Preserves folder structure and filenames; assigns ULIDs.
- Converts `> [!kind] Title` callouts to `::: {.callout-kind title="Title"}`.
- Keeps wikilinks; rewrites `![[img.png]]` image embeds to `![](img.png)`; other embeds
  are kept as `![[…]]` for tier-3 transclusion.
- Imports `.obsidian/bookmarks.json`, daily-notes settings, and `templates` folder.
- Copies attachments into content-addressed storage, keeps projected filenames.
- Ignores `.obsidian/` otherwise; reports unsupported syntax in a summary.

---

## 12. Export

Server-side (or local when the binary is installed) pandoc/quarto:

- Note → HTML, PDF (via LaTeX or typst), DOCX, reveal.js slides, Beamer slides.
- Vault or folder → zip of markdown + attachments (always available, no pandoc needed).
- `.qmd` → `quarto render` with the note's own front matter.
- Export options: include front matter, resolve wikilinks to relative paths, embed images.
- A per-vault `export/` folder may hold `defaults.yaml`, `template.tex`, `references.bib`,
  `style.csl`; the export uses them when present.

---

## 13. API, CLI, MCP

### 13.1 REST API (`/api/v1`)

```
GET    /vaults                                 list vaults
GET    /vaults/:v/notes?path=&tag=&q=          list / search
POST   /vaults/:v/notes        {path, content}  create
GET    /vaults/:v/notes/:id                    metadata + content (markdown)
PUT    /vaults/:v/notes/:id    {content}        replace content (applied as a diff-edit)
PATCH  /vaults/:v/notes/:id    {path}           rename/move
DELETE /vaults/:v/notes/:id                    trash
GET    /vaults/:v/notes/:id/backlinks
GET    /vaults/:v/notes/:id/versions[/:seq]
POST   /vaults/:v/notes/:id/export {format}
GET    /vaults/:v/daily/:date                  get-or-create daily note
GET    /vaults/:v/tags
PUT    /vaults/:v/attachments/:hash
GET    /vaults/:v/attachments/:hash
```

`PUT` content is never a blind overwrite: the server diffs against the current text and
applies the result as CRDT edits, so API writes merge with concurrent editors.

### 13.2 CLI

`notes login`, `notes vault ls`, `notes ls|cat|new|edit|mv|rm`, `notes search`,
`notes daily [date]`, `notes export`, `notes import obsidian`, `notes sync` (native
projection folder without the GUI). JSON output with `--json` for scripting.

### 13.3 MCP

- Transport: **Streamable HTTP** at `/mcp` on the server (auth by personal access token),
  and `notes mcp` for stdio against a server.
- Tools: `search_notes`, `read_note`, `write_note` (diff-merged), `append_to_note`,
  `list_notes`, `get_daily_note`, `get_backlinks`, `list_tags`.
- Resources: `note://<vault>/<path>` for read-only exposure.
- The CLI + this document double as the "skill" for agents that prefer shell access.

---

## 14. Platforms

| Platform | Shell | Offline | Projection | Notes |
|---|---|---|---|---|
| Linux / macOS / Windows | Tauri 2 | full | yes, watched | Primary target. |
| Android | Tauri 2 | full | yes, SAF folder, reconciled on foreground | |
| iOS | Tauri 2 | full | yes, Files app | Background sync limited by OS. |
| Web | browser | cached docs only | no | Served by the server; no install. |

Minimum: single-binary server on Linux amd64/arm64; Docker image; `fly.toml` with a
persistent volume for `notes.db` and attachments.

---

## 15. Non-goals (v1)

- Plugin system or scripting inside the app.
- Peer-to-peer sync, or sync via third-party file sync (Syncthing/Dropbox) — those will
  *work* on the projected files but are unsupported and will lose the CRDT benefits.
- End-to-end encryption. The server can read notes (required for search, sharing, MCP,
  public links). Encrypt the disk.
- Graph view, canvas, kanban, Dataview-style queries, spaced repetition.
- Executing Quarto/Jupyter cells in-app.
- WYSIWYG rich-text mode.
- Multi-server federation.

---

## 16. Milestones

**M0 — Foundations**
`core` crate: yrs docs, SQLite persistence, sync protocol, projection + watcher, markdown
subset parser with conformance corpus shared with the JS parser. CLI `sync` against a
local server. No UI.

**M1 — Single-user desktop MVP**
Tauri desktop, CM6 live preview with the full §5 subset, tree, tags, wikilinks +
backlinks, tabs/panes, quick switcher, command palette, FTS search, daily notes +
templates, attachments, bookmarks, Obsidian import, zip export. Server sync between two
desktops. Usable as a daily driver.

**M2 — Multi-user and web**
Accounts, OIDC, vault roles, per-note shares, public links, real-time cursors/presence,
version history, trash, web client, Docker + fly.io recipe.

**M3 — Mobile and power features**
Android/iOS apps with projection, pandoc export (PDF/slides/HTML/DOCX), citations,
`.qmd` awareness + quarto render, REST API, CLI, MCP, embeds/transclusion.

---

## 17. Open questions

1. **[open]** UI framework: Svelte 5 is recommended above; confirm or choose otherwise
   before M1 starts. CM6 and Yjs are framework-independent either way.
2. **[open]** Attachment placement in the projection: single `attachments/` at vault root
   (recommended, matches Obsidian default) vs per-folder `_attachments/`.
3. **[open]** Should `id:` be written into front matter by default (robust external
   renames, slight file noise) or only on demand (recommended)?
4. **[open]** Update-log retention (90 days proposed) vs storage on small servers.
5. **[open]** Citations: a single vault-level `references.bib` (recommended) or
   per-note `bibliography:` front matter — or both.
6. **[open]** Is a Vim keymap required for M1 or later?
7. **[open]** Which Obsidian plugins are in current use — any behaviour they provide that
   is not covered above?
