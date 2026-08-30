# notes — user guide

`notes` is a self-hosted, multi-user markdown notebook: you write pandoc-flavoured markdown
with wikilinks, tags, maths and attachments, and a small Rust server keeps every device and
collaborator in sync in real time. Editing is CRDT-based, so offline edits, edits from two
laptops, and edits from another person merge without conflict markers. There is no plugin
system and no query language — the extension points are the HTTP API, the CLI, and (soon) MCP.

**Your notes are files** (SPEC §1.1). A vault is always materialised as a plain folder of
`.md`/`.qmd` files plus an `attachments/` folder. You can `grep` it, `git init` it, edit it in
vim while the app is running, or walk away from the app entirely — leaving never requires an
export step. The CRDT is the source of truth and the file is its projection; external edits to
the files are diffed and absorbed as ordinary edits.

Three clients, one engine:

| Client | What it is | Offline |
|---|---|---|
| **Desktop** (`lemmate-desktop`) | Tauri 2 window over a **local relay**: the sync engine runs on your machine, owns the vault folder, and serves the same web UI on loopback. | Yes — full local copy, local search, edits journalled and pushed on reconnect. |
| **Web** | The same Svelte UI served by `lemmate-server`, talking to it over WebSocket + REST. | No. There is no IndexedDB cache yet, so a reload while disconnected loses unsent edits. |
| **CLI** (`notes`) | `lemmate sync` runs the same engine headlessly for a folder; plus indexing, search, import and export. | Yes, same engine. |

---

## 1. Getting started

### (a) Run a server

See [`deploy.md`](deploy.md) for Docker, a Caddy reverse proxy, fly.io, and backups. The short
version:

```sh
lemmate-server --data-dir ./data --web-dir ui/dist     # accounts on by default
```

The **first account to register becomes the admin**; afterwards only admins create accounts,
unless the server was started with `--allow-registration`. Register immediately after
deploying — on a fresh server the first registration succeeds without credentials.

`--no-auth` disables accounts, roles and permission checks entirely. It is a **development
switch only**: every request is treated as a local owner. Never expose a `--no-auth` server to
a network.

### (b) Desktop app, first run

`lemmate-desktop` reads `$XDG_CONFIG_HOME/notes/desktop.toml` (falling back to
`~/.config/lemmate/desktop.toml`). With no config file it opens a **setup screen** asking for:

- **Vault folder** — where your `.md` files live on this computer (created if missing);
- **Server URL** — e.g. `https://notes.example.org`;
- **Vault id** — leave empty to create a new vault, or paste a ULID to join an existing one;
- **Email / password**, with a "create this account" checkbox for the first account on a new
  server. Leave both empty for a `--no-auth` server.

Submitting signs in, writes `desktop.toml`, starts the relay and opens the vault. Every key has
a flag (`--vault-dir`, `--server-url`, `--vault-id`, `--ca-cert`, `--web-dir`, `--config`) and
most have an environment variable; see [`../crates/desktop/README.md`](../crates/desktop/README.md).

The window is the web client served by the embedded relay, so it keeps working with the server
unreachable.

### (c) Sync a folder from the command line

```sh
lemmate login --server https://notes.example.org --email you@example.org --register
lemmate sync  --vault ~/vault --server https://notes.example.org          # keeps running
```

`login` stores a session token in `~/.config/lemmate/credentials.toml` (mode 0600); `sync` picks
it up automatically. First run **publishes** the folder as a new vault and prints the id; to
join an existing vault into an empty folder, pass `--vault-id <ULID>`. Add `--once` to sync and
exit. Add `--serve 127.0.0.1:8081 --web-dir ui/dist` to also run the local relay, which serves
the sync socket, the API and the web client on loopback — this is exactly what the desktop app
embeds.

For a private CA, `--ca-cert ca.pem` (or `LEMMATE_CA_CERT`); `--server https://…` implies `wss://`.

### (d) Web client

Open the server's URL, sign in, and pick a vault (or create one). The URL hash is the route:
`#/v/<vault>` for a vault, `#/n/<vault>/<note>` for a note shared directly with you, and
`#/s/<token>` for a public read-only link.

---

## 2. Writing

The dialect is a fixed subset of pandoc markdown (SPEC §5). Anything outside it is preserved
verbatim — the editor never rewrites syntax it does not understand.

### Front matter

```yaml
---
id: 01J8Z9K3M7QRSTVWXYZ0123456      # written for you; do not edit
title: Reading list                  # overrides the filename as the display title
tags: [reading, projects/alpha]      # merged with inline #tags
aliases: [books, to-read]            # alternative link targets
date: 2026-08-30
---
```

`id:` is added automatically — by the editor when you create a note, and by the sync engine on
first sync for files you made by hand. It is what survives a `mv`: with an `id:`, moving a file
outside the app is recognised as a rename rather than a delete plus a create. Unknown keys pass
through untouched.

### The rest of the dialect

```markdown
# Heading   ## Heading   ### Heading
*emphasis*, **strong**, ~~strikeout~~, `inline code`

[[Other Note]]                  wikilink; resolves by path, then basename
[[Other Note#Section|label]]    heading fragment and a custom label
![[diagram.png]]                embedded image
![[Other Note]]                 note transclusion — parsed, but not yet rendered
[label](relative/path.md)       ordinary links work too
#tag and #nested/tag            not inside code or maths; case-insensitive

Inline maths $e^{i\pi}+1=0$, and display maths on its own:

$$ \int_0^1 x^2 \, dx = \tfrac13 $$

- [ ] a task
- [x] a finished task

::: {.callout-note title="Heads up"}
Callout body. Kinds: note, tip, warning, caution, important.
:::

| a | b |
|---|---|
| 1 | 2 |

A footnote[^1] and a citation [@knuth1984].

[^1]: The note text.
```

### What renders where

Live preview hides markup and draws the result in place; the markup comes back on any line the
cursor or selection touches. Rendered **in the editor**: headings, emphasis/strong/strikeout,
inline code, links, images, wikilinks and image embeds, `$…$` and `$$…$$` via KaTeX, `#tags`,
task checkboxes, blockquotes, fenced code blocks, callout blocks, and front matter (folded to a
one-line property summary — click it to edit).

Recognised by the indexer and handled by pandoc **on export only**, with no editor decoration
today: footnotes, citations, definition lists, superscript/subscript, bracketed spans, header
and link attributes. Tables are indexed and exported, but in the editor they are only styled as
monospace rows — there is no rendered grid or cell-by-cell editing yet. `![[note]]`
transclusion is parsed but shown as a plain link; only attachment embeds render inline.
Callout `collapse="true"` is not implemented.

### Editing behaviour

- **Live preview only.** The source/reading-mode toggle in SPEC §8 is not built yet.
- **Checkboxes are clickable** — clicking a rendered box rewrites `[ ]` ↔ `[x]` in the source.
- **Autocomplete**: type `[[` for note paths, `#` for existing tags. The `@` citation, `:::`
  callout and `/` slash menus described in SPEC §8 are not implemented.
- **Front matter opens folded** and the cursor lands after it.
- Standard CodeMirror editing: undo/redo, find (`Ctrl+F`), bracket matching, `Tab` indent.
- `.qmd` files are first-class notes with the same editor, links and search.

---

## 3. Organising

**Folders are real folders.** The tree mirrors the vault directory, shows a note count per
folder, and remembers which folders you collapsed. Moving, creating and deleting from the tree
by drag-and-drop is not built yet — use *Rename / move* (which takes a full path) or move the
file on disk.

**Quick switcher** (`Ctrl+O`, `Ctrl+P`, `Ctrl+N`) fuzzy-matches paths; substring matches rank
above subsequence matches. If nothing matches exactly, the last entry offers to **create** the
note at that path (`.md` appended unless you typed `.md`/`.qmd`).

**Command palette** (`Ctrl+Shift+P`) lists every command with its shortcut. Shortcut remapping
is not implemented.

**Sidebar panes**: Files (tree), Search, Tags, Outline, Bookmarks (★), Version history (⏱).

- **Search** is SQLite FTS5 over title and body, with highlighted snippets. FTS5 syntax works
  (`"exact phrase"`, `OR`, `NOT`, `prefix*`); the `tag:`, `path:`, `has:math` filters in
  SPEC §10 are not implemented yet.
- **Tags** shows every tag with a count; clicking one lists its notes, including nested tags
  under it (`#projects` matches `#projects/alpha`).
- **Outline** lists the current note's headings; click to jump.
- **Backlinks** appear under the editor as "Linked from". They match links whose target is the
  note's full path, its path without extension, or its basename. Unlinked mentions and an
  outgoing-links pane are not built.
- **Bookmarks** live in the vault doc, so they follow you to every device. `Ctrl+Shift+B`
  toggles a bookmark for the current note.

**Daily notes.** `Ctrl+Shift+D` opens (or creates) `Daily/YYYY-MM-DD.md` for today. The path
and format are fixed at present — the per-vault configuration, prev/next-day navigation and
calendar popover in SPEC §9 are not built.

**Templates.** Put `Templates/Note.md` and `Templates/Daily.md` in the vault; they are applied
when a note or a daily note is created (the template's own front matter is stripped first).
Variables:

| Variable | Expands to |
|---|---|
| `{{date}}` | `YYYY-MM-DD` today |
| `{{date:FORMAT}}` | today with `YYYY`, `MM`, `DD`, `HH`, `mm` substituted |
| `{{time}}` | `HH:mm` now |
| `{{title}}` | the new note's display name (the date, for a daily note) |
| `{{cursor}}` | removed — the cursor is not repositioned yet |

Without a template, a new note starts as `# <title>`. There is no scripting.

**Attachments.** Paste or drop files into the editor: they are uploaded content-addressed
(blake3) and referenced at the cursor — images as `![[name.png]]`, everything else as
`[name](attachments/name)`. They land in the vault-level `attachments/` folder, with a
`-<hash6>` suffix if the name collides. Only files a note actually references are synced;
unreferenced files in the folder are ignored. Deleting an attachment that is still referenced
restores it — drop the reference to drop the file.

**Tabs and panes.** Each pane has its own tab strip, editor and backlinks. `Ctrl+\` splits
right (up to three panes); `Ctrl+Alt+←/→` moves focus. Pinned tabs sort first and ignore
`Ctrl+W` (unpin from the palette to close them). `Ctrl+Shift+T` reopens the last closed tab —
the last twenty are remembered. The layout, pins and collapsed folders are stored per vault in
the browser's local storage, so they are per device, and tabs pointing at notes that no longer
exist are dropped once the vault has synced.

**Version history** (⏱ pane) lists snapshots for the open note — automatic ones (taken every
500 updates or 10 minutes) plus any you name with *Save version…*. Click one to preview it,
then *Restore*: restoring is applied as one more edit, so nothing in the history is lost.
Snapshots are kept forever; the raw update log behind them is pruned after `--retain-days`
(90 by default).

**Trash.** *Delete* removes the note from the vault doc; the file disappears from every synced
replica and the tab closes everywhere. The note's update log and versions stay in the store, so
nothing is destroyed — but there is no restore-from-trash view in the UI or CLI yet, so
recovering a deleted note today means going through the database. On the server, attachments
that no note references any more are purged after a grace period
(`--attachment-grace-days`, 30 by default); referencing one again before then rescues it.

---

## 4. Collaboration

**Accounts.** Email + password, sessions as opaque bearer tokens (hashed at rest) — sent as
`Authorization: Bearer …` by native clients and as an HttpOnly cookie by the browser. OIDC is
specified but not implemented.

**Vault roles**: **owner** (manages members), **editor** (read + write), **viewer** (read
only). They are enforced on every REST call and every relay frame, not in the UI: a viewer's
updates are refused by the server. A vault nobody owns yet is claimed by the first user who
syncs it, so create and claim your vaults before handing out accounts.

**Sharing a whole vault** is a REST-only operation today — there is no members UI:

```sh
curl -H "Authorization: Bearer $TOKEN" https://…/api/v1/vaults/$VAULT/members
curl -X PUT -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
     -d '{"email":"them@example.org","role":"editor"}' https://…/api/v1/vaults/$VAULT/members
curl -X DELETE -H "Authorization: Bearer $TOKEN" https://…/api/v1/vaults/$VAULT/members/$USER
```

**Sharing one note** — the *Share* button in the note header, or *Share note…* in the palette:

- **With a person**, by the email of an existing account, as *can view* or *can edit*. They see
  it under "Shared with me" on the vault picker, and it opens in a single-pane view with just
  that note — a note share grants the note, not the vault.
- **As a public read-only link**. The URL (`…/#/s/<token>`) is shown once; anyone with it reads
  the note without logging in, rendered read-only with markup always folded. *Revoke* kills
  every link for that note. The API supports link expiry (`expires_days`); the dialog does not
  expose it yet.

Sharing needs a server with accounts. Against the desktop's local relay the dialog reports
"Sharing is not available here".

**Presence and cursors.** Everyone editing the same note sees the others' cursors and
selections with name labels (colour derived from the name). The note header shows "· with N
others" and the sidebar footer shows how many people are editing. Your display name comes from
your account.

---

## 5. Keyboard shortcuts

`Ctrl` is `Cmd` on macOS. These are the shortcuts the app itself binds:

| Shortcut | Action |
|---|---|
| `Ctrl+O` / `Ctrl+P` | Open or create note (quick switcher) — toggles |
| `Ctrl+N` | New note (quick switcher; type a path, Enter creates) |
| `Ctrl+Shift+P` | Command palette |
| `Ctrl+Shift+F` | Search pane |
| `Ctrl+Shift+D` | Today's daily note |
| `Ctrl+Shift+B` | Bookmark / unbookmark this note |
| `Ctrl+W` | Close tab (no-op on a pinned tab) |
| `Ctrl+Shift+T` | Reopen closed tab |
| `Ctrl+\` | Split right |
| `Ctrl+Alt+→` / `Ctrl+Alt+←` | Focus next / previous pane |

Inside the quick switcher and palette: `↑`/`↓` to move, `Enter` to choose, `Escape` to close.

Commands without a shortcut, reachable from the palette: show Files / Tags / Outline /
Bookmarks / Version history, Share note…, Rename / move note, Move note to trash, Pin /
unpin tab, Close pane, Switch vault, Sign out.

Inside the editor, CodeMirror's own bindings apply — `Ctrl+F` find, `Ctrl+Z` / `Ctrl+Y` undo
and redo, `Tab` indent, `Ctrl+Space` autocomplete. There is no vim keymap (SPEC §17).

---

## 6. Command line

```
notes <command>
```

| Command | What it does |
|---|---|
| `lemmate login --server URL --email E [--register] [--ca-cert F]` | Sign in (or create the account) and save the token to `~/.config/lemmate/credentials.toml`. Password prompted if not given. |
| `lemmate logout --server URL` | Forget the saved token for that server. |
| `lemmate sync --vault DIR --server URL [--vault-id ULID] [--once] [--serve ADDR --web-dir DIR] [--ca-cert F] [--token T]` | Keep a folder in sync; optionally run the local relay and serve the web client. |
| `lemmate index PATH [--json]` | Index one file or a whole vault and print what the engine extracts (title, tags, links). |
| `lemmate search VAULT QUERY [--limit N]` | Full-text search over a vault directory, using a throwaway in-memory index. |
| `lemmate import obsidian SRC --into DIR [--overwrite]` | Import an Obsidian vault (see §8). |
| `lemmate export zip VAULT OUT` | Zip the vault's notes and attachments. No pandoc needed. |
| `lemmate doctor` | Print versions, the SQLite schema version, and whether `pandoc`/`quarto` are on `PATH`. |

`LEMMATE_SERVER`, `LEMMATE_TOKEN`, `LEMMATE_CA_CERT`, `LEMMATE_PASSWORD` and `LEMMATE_WEB_DIR` back the
corresponding flags.

The remote commands from SPEC §13.2 (`lemmate ls|cat|new|mv|rm`, `lemmate daily`, `notes vault ls`)
and the stdio MCP server (`lemmate mcp`, SPEC §13.3) are being wired up now; when they land, the
CLI's own `crates/cli/README.md` documents the MCP tool surface.

---

## 7. Export

`POST /api/v1/vaults/{vault}/notes/{id}/export` with `{"format": …}` renders a note through
**pandoc**:

| Format | Output |
|---|---|
| `html` | standalone HTML (maths via MathJax) |
| `docx` | Word document |
| `pdf` | PDF — needs a LaTeX engine next to pandoc |
| `revealjs` (or `slides`) | reveal.js slide deck |
| `beamer` | Beamer PDF slides |
| `markdown` (`md`, `commonmark`) | normalised markdown |

Pandoc reads the note as
`markdown+wikilinks_title_after_pipe+tex_math_dollars+fenced_divs+bracketed_spans`, so
wikilinks, `$…$`, callouts and bracketed spans survive the trip. Front matter is stripped
before rendering. Pandoc is found on `PATH` unless you pass `--pandoc PATH` / `LEMMATE_PANDOC`;
without it the endpoint answers **501**. `lemmate doctor` tells you whether it is installed.

A vault-level `export/` folder is consulted when the exporter knows the vault directory:
`defaults.yaml` (passed as `--defaults`), `references.bib` (turns on `--citeproc`), and
`style.csl` next to it (`--csl`). Note that the **server-side** export path does not currently
pass a vault directory, so today `export/` and image resource paths only apply where the
exporter is given one — server exports render the note text alone, with links left relative.
The local relay has no export endpoint at all yet, so exporting requires the server.

Whole-vault export never needs pandoc: `lemmate export zip <vault> <out.zip>` writes the markdown
and attachments as they are. And because the vault is already a folder of files, `pandoc` or
`quarto` can be pointed straight at it.

---

## 8. Coming from Obsidian

```sh
lemmate import obsidian ~/ObsidianVault --into ~/vault
lemmate sync --vault ~/vault --server https://notes.example.org
```

The importer preserves folders and filenames and reports what it did. What changes:

| Obsidian | Here |
|---|---|
| `> [!note] Title` callouts | Converted to `::: {.callout-note title="Title"}` fenced divs |
| `![[image.png]]` embeds | Rewritten to `![](image.png)`; non-image embeds stay `![[…]]` |
| `[[wikilinks]]`, `[[a\|b]]`, `#tags`, maths, front matter | Left exactly as they are |
| Self-hosted LiveSync | The built-in sync — one WebSocket, CRDT merge, no conflict files |
| File Tree Alternative | The built-in tree, with per-folder note counts |
| `.obsidian/bookmarks.json`, `daily-notes.json` | Translated into `.lemmate/*.import.json`; nothing consumes them yet, so re-create bookmarks and check the daily-note path by hand |
| `.obsidian/`, `.trash/` | Skipped |

Note ids are not written during import — the sync engine assigns them on first sync.

Deliberately absent, and not planned (SPEC §15): plugins and in-app scripting, graph view,
canvas, kanban, Dataview-style queries, spaced repetition, WYSIWYG rich text, end-to-end
encryption, and peer-to-peer sync.

---

## 9. Troubleshooting

**The status dot.** Bottom-left of the sidebar: green when the socket is `online`, amber when
`connecting` or `offline`, followed by the note count and, while the vault doc is still
catching up, "syncing…". Reconnection is automatic with exponential backoff up to 30 s.

**Offline.** On desktop and `lemmate sync`, everything keeps working: edits are journalled in
`<vault>/.lemmate/local.db` and reconciled when the server returns. In the **browser** there is
no local persistence yet — a reload while offline loses anything unsent, so avoid reloading a
disconnected tab.

**Conflicts never produce markers.** Two people, two devices, or a device and an external
editor can all edit the same note at once; the CRDT merges the results. External file edits are
diffed against the last projected text, so they compose with concurrent edits instead of
overwriting them. Two notes concurrently moved to the same path get a ` (2)` suffix.

**"Permission denied" on a doc.** The relay refuses `sync1` on a note you cannot read and drops
updates on one you can only view. The UI does not surface that message yet, so the symptom is a
note that stays empty or edits that quietly do not stick. Check your role on the vault
(`GET /api/v1/vaults/{v}/members`) — a viewer cannot write.

**Where things live.**

| Path | What |
|---|---|
| `<vault>/.lemmate/local.db` | Local update log, snapshots, index, and the vault id for this folder |
| `<vault>/.lemmate/attachments/` | Content-addressed attachment cache |
| `<vault>/attachments/` | The human-readable projection of referenced attachments |
| `~/.config/lemmate/credentials.toml` | Saved session tokens, one per server (mode 0600) |
| `~/.config/lemmate/desktop.toml` | Desktop app configuration |
| `<data-dir>/lemmate.db`, `<data-dir>/attachments/` | Everything on the server |

**Resetting a device.** The sidecar is a cache, not your data: stop the client, delete
`<vault>/.lemmate/`, and re-sync. Pass `--vault-id <ULID>` (the id is printed by `lemmate sync
--once`, appears in the vault URL, and is what `desktop.toml` stores) so the folder rejoins the
same vault instead of publishing itself as a new one. Any local-only edit that never reached
the server is lost with the journal, so sync before you delete it. To start completely clean,
delete the vault folder as well and let the engine re-materialise it from the server.

**Login appears to do nothing** over HTTPS: the server needs `--secure-cookies`
(`LEMMATE_SECURE_COOKIES=true`) so the browser stores the session cookie — and must *not* have it
on a plain-HTTP deployment. See [`deploy.md`](deploy.md).
