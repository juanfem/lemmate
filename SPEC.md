# Lemmate — Specification

Status: draft v0.4 (2026-08-30) — M0–M2 implemented, accounts through single-use invites and
password changes (§11.1); M3 partly (export, REST/relay writes, MCP, remote CLI), with mobile and
Quarto render outstanding; see README status
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
  **Svelte 5 [decided]** (small bundle, matters on mobile webviews). Identical bundle in
  desktop, mobile, and web.
- **`desktop`, `mobile`** — Tauri 2 shells. Both run `core`'s local relay in-process and point
  the webview at it over loopback HTTP, rather than exposing `core` through Tauri commands
  **[decided: built that way]** — the UI then speaks one protocol to a server, a relay and a
  phone alike, and the bundle stays identical. Nothing crosses Tauri IPC. The mobile shell
  additionally compiles the web assets in, because an APK's resources are not files a static
  file server can open.
- **`cli`** — `lemmate` binary. Talks to a server over the REST API, or to a local vault
  directly via `core` **[recommended: server-only in v1, direct-local later]**.

### 3.2 Topology **[decided]**

- Client ↔ server only. **No peer-to-peer.** Every device syncs through the server it is
  logged into. A vault lives on exactly one server.
- Native clients (desktop, mobile) are **offline-first**: full local copy, full local search,
  edits queue and merge on reconnect.
- The web client is **online-first** for anything the server computes — search, backlinks,
  tags, trash, history, sharing. Its notes depend on how it is being run
  **[decided: revised]**:
  - **Installed** (home screen, app window): every note is pulled into IndexedDB in the
    background, so the whole tree opens with no network, and edits queue and merge like a
    native client's. This once said "recently opened notes are editable offline, the rest
    require connectivity", which in practice meant a complete file tree of which almost nothing
    would open — an installed web app on a phone made that the common case, not the corner one.
  - **A browser tab**: only what has been opened, as before. Installing is a deliberate act on
    a device you own; a tab is often someone else's machine, and hoarding a copy of the vault
    there — on their bandwidth — is not a favour.

  Search follows the same split: `/api/v1/search` whenever the server answers, and an offline
  index over the cached notes when it does not (§6.4). What separates even the installed case
  from a native client is the *engine*: no SQLite, no watcher, and no projection to files —
  which mobile no longer wants anyway (§6.3).

### 3.3 Technology choices

| Concern | Choice | Notes |
|---|---|---|
| CRDT | Yjs (`yjs` in TS, `yrs` in Rust) **[decided: CRDT truth]** | Mature CM6 binding (`y-codemirror.next`), awareness protocol for cursors, identical wire format across languages. |
| Editor | CodeMirror 6, live-preview decorations **[decided]** | Source is always the document; decorations render previews in place. |
| Server | Rust, axum, tokio | Single binary, shares `core`. |
| Database | SQLite (WAL) | Per server, not per vault. Attachments outside the DB. |
| Search | SQLite FTS5, trigram + unicode61 | Same engine on server and native clients. |
| Native shells | Tauri 2 **[decided: native mobile]** | Linux/macOS/Windows/Android/iOS from one codebase. |
| Markdown parser (JS) | micromark + mdast with custom extensions | Used by editor decorations, link/tag extraction on the client. |
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
- `attachments: Y.Map<path, hash>` — vault-relative path → blake3 of the blob (§7).
- `meta: Y.Map<string, string>` — vault-level settings shared by every replica; today only
  `name`, the label the UI shows for the vault (everywhere else a vault has an id and no name,
  and clients fall back to a short form of the id).

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
id: 01J…          # written by the app on creation/import [decided]; see §6.3
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

- `lemmate.db` (SQLite, WAL): all tables in §4.1. Doc updates are appended per doc; a
  snapshot is written every 500 updates or 10 minutes, after which older updates may be
  pruned to the last snapshot older than the version-history retention window (§9).
- `attachments/<vault_id>/<hash[0..2]>/<hash>` on the filesystem.
- `exports/` scratch for pandoc output, TTL 1 hour.
- Backup = `sqlite3 .backup` + `rsync attachments/`. Documented as a one-liner.

### 6.2 Native clients

Per vault, a local directory chosen by the user (the projection, §6.3) plus a sidecar:

```
<vault>/
  .lemmate/
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
- Attachments referenced by a note are materialised under a single vault-level
  `attachments/` folder **[decided]** with `<filename_hint>` (deduplicated with
  `-<hash[0..6]>` on collision); notes may still reference files anywhere in the vault.

Read direction (disk → CRDT):
- A watcher (`notify` crate) observes the vault directory, ignoring `.lemmate/`.
- On change to a known file: compute a diff between the last projected text (stored in
  `local.db`) and the new file content, apply the diff as a Y.Text delta. Because the diff
  is against the *last projected* text, it composes correctly with concurrent CRDT edits.
- New file: create note with new ULID at that path.
- Deleted file: soft-delete note (trash, §9); a delete is never propagated destructively
  from disk without a trash stop.
- Moved file: if a new file's content hash equals a just-deleted file's hash within 2 s,
  treat as rename; otherwise delete + create.
- The `id:` front-matter field is written by default when a note is created or imported
  **[decided]** and takes precedence for identity, so external tools (and plain `mv`) can move
  files without the content-hash heuristic. Hand-made files without one still get the
  heuristic and gain an `id:` on first sync.

**Mobile does not project [decided].** The vault lives in the app's own storage as CRDT and
nothing else; there are no `.md` files for the rest of the phone to see. This once promised the
Storage Access Framework on Android and the Files app on iOS, and dropping that costs less than
it appears to: the projection exists so that *other tools* can work on the notes — an editor, a
script, an LLM writing into the folder, a plain `mv` — and those are desktop habits. Nobody
points a language model at a directory on their phone. What is left on mobile once the files
are gone is the app itself, which reads and writes the CRDT directly.

It also removes the least reliable part of the mobile design. Background watching does not work
on either platform, so the plan had been to reconcile the folder on every foreground: a
conflict-detection pass, on a device whose OS may have killed the app mid-write, guarding files
that in practice only that same app was ever going to touch.

### 6.4 Web client

`y-indexeddb` caches docs the user has opened; the vault doc is always cached. No file
projection. Attachments are fetched on demand.

Note content is prefetched in the background once the vault doc has synced, and only when the
client is installed (`display-mode: standalone` and friends, or iOS's `navigator.standalone`) —
one note at a time, skipping what is already stored, abandoned the moment the socket drops and
resumed on the next sync. The Tauri shells report a browser display mode and are excluded,
correctly: their relay already holds the vault on local disk. It does not keep those copies fresh: a note changed on another device updates here
when it is opened. Keeping every cached doc current would mean subscribing the whole vault
permanently, which is a different design.

Edits made offline outlive the view that made them. A note doc is only pushed while it is
subscribed, so a note edited and then closed had nowhere to go — reconnecting re-handshook
whatever was still open and left that one in IndexedDB until someone reopened it. Ids with
unacknowledged local changes are kept in localStorage, stay subscribed until the server
acknowledges, and are re-subscribed on the next start. The vault doc needs none of this: it is
subscribed for the life of the session, so notes *created* offline arrive with it.

Offline search runs over those cached copies. Each is indexed on arrival with
`markdown/index.ts` — the same parser, over the same title and `plain_text` the server puts in
its FTS — and the result is kept in IndexedDB, which doubles as the record of what is cached and
at which version. Matching is by substring rather than FTS5's tokens, and ranking is occurrence
counts weighted towards titles rather than bm25, so offline results are broader and more crudely
ordered than the server's; the pane says it is offline while this is in use. The parser is a
lazily-loaded chunk and is not precached: a browser tab never indexes anything and should not
pay 185 kB for the machinery.

A service worker precaches the built shell so the app starts with no network, and a web app
manifest makes it installable — on iOS an installed web app is also exempt from Safari's
seven-day eviction of unused storage, which is what makes the cache above worth having. The
worker never touches `/api/` or `/ws`: those must fail honestly so the client shows its offline
state rather than replaying a stale answer, and the notes come from the CRDT docs regardless.
The last known vault ids are kept alongside, because sessions are built from the vault list and
without one there is nothing to open the cached docs *with*.

Offline is therefore: read and edit what you have opened, reconciling on reconnect. Search,
backlinks, tags, trash, history, sharing and un-fetched attachments are all server-side and go
dark — the gap between this and a native client (SPEC §3.2) is by design, not by omission.

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
**source** (no decorations), **reading** (everything rendered, read-only). The mode belongs to
the **pane**, so a source pane can sit beside a reading one, and it is saved with the layout.

All three are the same CodeMirror view reconfigured through a compartment, not three renderers:
reading mode is live preview with the reveal-on-cursor rule switched off and the view made
read-only. An mdast-to-HTML reading mode was the earlier plan; it was dropped because a second
renderer would have to be kept in step with the decorations for every construct in §5, and
public links (§11.2) already prove the CodeMirror one reads well without a cursor.

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
- Find/replace in note; multi-cursor. (Vim keymap: not planned for M1 **[decided]**.)
- Spellcheck via the platform webview.
- Collaboration: remote cursors and selections with name labels; presence list per note.
- Mobile: toolbar row above the keyboard for markup, indent, checkbox, link, image.

---

## 9. Organisation and workflow features

- **Tree** — every vault you can read is a root of one tree, with its folders below it;
  folders are real folders. Create, rename, delete. Sort by name/modified. Optional folder note
  (`<folder>/<folder>.md`).
- **Selecting and moving** — click selects and opens, Ctrl/Cmd-click adds, Shift-click takes
  the range as drawn on screen. Notes and folders drag onto any folder or vault root; a
  right-click menu offers the same moves by name. Inside one vault a move is a rename and
  `[[links]]` follow it (§4.4). Between vaults it cannot be — a note id is an entry in one
  vault doc — so the note is re-created there with a new id, the attachments it references are
  copied, the original is trashed, and the user confirms that trade first.
- **Two file browsers over the same folders** — the interleaved tree above, and a folder-first
  split (folders on top, the selected folder's notes below, optionally reaching into its
  subfolders) for the *File Tree Alternative* workflow in §11. Both share one set of folds;
  the choice, the folds, the selected folder and the two split sizes are per device.
- **One workspace** — vaults are not opened one at a time: tabs may hold notes from different
  vaults, the quick switcher lists every vault's notes, and search runs across all of them
  (§10). Panes that can only be per-vault — tags, version history, trash, sharing — follow the
  focused note's vault. One WebSocket carries every vault: the frame protocol is addressed by
  doc id (§7), so a connection is not bound to one.
- **Tabs and panes** — multiple open notes, split horizontally/vertically, pinned tabs,
  reopen closed tab. Tab state persisted per device. Opening a note **reuses the focused
  pane's active tab** (browsing is not tab-creation); a pinned tab and one already showing the
  note are never displaced, and a displaced tab joins the reopen stack. New tabs are explicit:
  the ＋ on the strip, or *Open in a new tab* in the browser's right-click menu.
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
  (configurable) **[decided]**, versions retained forever.
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
- **Registration has four doors** and no others: the user table is empty (that account becomes
  the admin), `--allow-registration` is on, an admin makes the request, or the request carries a
  valid invite.
- **Invites** are single-use registration links minted by an admin: a random token, stored only
  as its BLAKE3 hash, with an optional expiry. Redeeming one creates a non-admin account, and
  redeem-and-create is a single transaction so a link that two people open at once still yields
  one account. They are not bound to an email — whoever holds the link picks their own address —
  so a link is a credential and is handed out like one. Spent invites are kept, not deleted:
  they record which account each link created.
- **Passwords are changed, never mailed.** A user changes their own by proving the current one;
  an admin sets another user's without it. Either way every *other* session of that account is
  revoked. There is deliberately no reset-by-email flow: a self-hosted server has no mail (§15),
  so the admin reset is the recovery path.

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

`lemmate import obsidian <dir>` (CLI), and `POST /api/v1/vaults/{vault}/import` for the web and
desktop clients — a multipart body whose parts are the picked files, each named by its
vault-relative path, uploaded in batches so that a large vault is not one enormous request.
Importing into a vault nobody owns claims it, as a first sync does; a path the vault already
holds is skipped, so a repeated batch cannot duplicate notes. On a server the imported notes are
created through the room docs; on the local relay they are written into the vault folder, which
is also the only side with a sidecar to keep daily-note settings in.

Either way the conversion is the same code:
- Preserves folder structure and filenames; assigns ULIDs.
- Converts `> [!kind] Title` callouts to `::: {.callout-kind title="Title"}`.
- Keeps wikilinks; rewrites `![[img.png]]` image embeds to `![](img.png)`; other embeds
  are kept as `![[…]]` for tier-3 transclusion.
- Imports `.obsidian/bookmarks.json`, daily-notes settings, and `templates` folder.
- Copies attachments into content-addressed storage, keeps projected filenames.
- Ignores `.obsidian/` otherwise; reports unsupported syntax in a summary.
- Plugins in current use and what replaces them: *Self-hosted LiveSync* → the built-in sync
  (§7); *File Tree Alternative* → the folder-first tree pane with per-folder note counts and
  folder notes (§9) is the M1 target for that workflow.

---

## 12. Export

Server-side (or local when the binary is installed) pandoc/quarto:

- Note → HTML, PDF (via LaTeX or typst), DOCX, reveal.js slides, Beamer slides.
- Vault or folder → zip of markdown + attachments (always available, no pandoc needed).
- `.qmd` → `quarto render` with the note's own front matter.
- Export options: include front matter, resolve wikilinks to relative paths, embed images.
- A per-vault `export/` folder may hold `defaults.yaml`, `template.tex`, `references.bib`,
  `style.csl`; the export uses them when present. Citations resolve against the single
  vault-level `references.bib` **[decided]**; a per-note `bibliography:` override is an
  export-time feature for later.

---

## 13. API, CLI, MCP

### 13.1 REST API (`/api/v1`)

```
POST   /auth/register     {email, password, invite?}   create an account
POST   /auth/login | /auth/logout | GET /auth/me
POST   /auth/password  {new_password, current_password?, email?}  change or (admin) reset
GET    /invites                                list invites (admin)
POST   /invites        {expires_days?}          mint a single-use invite (admin)
DELETE /invites/:id                            revoke an unused invite (admin)
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

`lemmate login` (`--invite <link>` to redeem one), `lemmate vaults`,
`lemmate ls|cat|new|edit|mv|rm`, `lemmate search`, `lemmate daily [date]`, `lemmate export`,
`lemmate import obsidian`, `lemmate passwd` (own, or `--email` to reset another as admin),
`lemmate invite` (`--list`, `--revoke`), `lemmate sync` (native projection folder without the
GUI). JSON output with `--json` for scripting.

### 13.3 MCP

- Transport: **Streamable HTTP** at `/mcp` on the server (auth by personal access token),
  and `lemmate mcp` for stdio against a server.
- Tools: `search_notes`, `read_note`, `write_note` (diff-merged), `append_to_note`,
  `list_notes`, `get_daily_note`, `get_backlinks`, `list_tags`.
- Resources: `note://<vault>/<path>` for read-only exposure.
- The CLI + this document double as the "skill" for agents that prefer shell access.

---

## 14. Platforms

| Platform | Shell | Offline | Projection | Notes |
|---|---|---|---|---|
| Linux / macOS / Windows | Tauri 2 | full | yes, watched | Primary target. |
| Android | Tauri 2 | full | no — §6.3 | |
| iOS | Tauri 2 | full | no — §6.3 | Background sync limited by OS. |
| Web | browser | cached docs only | no | Served by the server; no install. |

Minimum: single-binary server on Linux amd64/arm64; Docker image; `fly.toml` with a
persistent volume for `lemmate.db` and attachments.

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

## 17. Resolved questions (2026-08-30)

| Question | Decision |
|---|---|
| UI framework | Svelte 5. |
| Attachment placement | Single vault-level `attachments/` folder. |
| `id:` in front matter | Written by default; renames/moves resolve by id. |
| Update-log retention | 90 days. |
| Citations | One `references.bib` per vault; per-note `bibliography:` as a later export feature. |
| Vim keymap | Not for now. |
| Obsidian plugins in use | File Tree Alternative, Self-hosted LiveSync — both covered by built-ins (§9, §7). |
