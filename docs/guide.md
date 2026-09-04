# Lemmate — user guide

Lemmate is a self-hosted, multi-user markdown notebook: you write pandoc-flavoured markdown
with wikilinks, tags, maths and attachments, and a small Rust server keeps every device and
collaborator in sync in real time. If you only ever write on one computer you can skip the
server entirely and run the desktop app standalone (§1b) — the sync engine is on your machine
either way. Editing is CRDT-based, so offline edits, edits from two
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
| **Desktop** (`lemmate-desktop`) | Tauri 2 window over a **local relay**: one sync engine per vault runs on your machine, each owning its folder, and they serve the same web UI on loopback. A server is optional (§1b). | Yes — full local copy of every vault, local search, edits journalled and pushed on reconnect. |
| **Web** | The same Svelte UI served by `lemmate-server`, talking to it over WebSocket + REST. | Yes, with limits — notes and unsent edits live in IndexedDB; install it (§1d) and the whole vault comes too, including offline search. |
| **CLI** (`lemmate`) | `lemmate sync` runs the same engine headlessly for a folder; plus indexing, search, import and export. | Yes, same engine. |

---

## 1. Getting started

Nothing is packaged yet, so the binaries come from a build: [`install.md`](install.md) has the
prerequisites and steps for Linux, macOS and Windows, and where each installed piece puts its
files.

### (a) Run a server — or don't

You need one to reach your notes from more than one device, to share anything, or to use the
web client and the phone. For one computer, skip to §1b and leave the server out: the desktop
app runs standalone and nothing goes on the network.

See [`deploy.md`](deploy.md) for Docker, a Caddy reverse proxy, fly.io, and backups. The short
version:

```sh
lemmate-server --data-dir ./data --web-dir ui/dist     # accounts on by default
```

The **first account to register becomes the admin**; afterwards accounts come from the admin —
either created outright or through an invite link (§4) — unless the server was started with
`--allow-registration`. Register immediately after deploying: on a fresh server the first
registration succeeds without credentials, so whoever gets there first is the admin.

`--no-auth` disables accounts, roles and permission checks entirely. It is a **development
switch only**: every request is treated as a local owner. Never expose a `--no-auth` server to
a network.

### (b) Desktop app, first run

`lemmate-desktop` reads `desktop.toml` from your configuration directory — `~/.config/lemmate` on
Linux, `~/Library/Application Support/lemmate` on macOS, `%APPDATA%\lemmate` on Windows (set
`LEMMATE_CONFIG_DIR` to put it somewhere else). With no config file it opens a **setup screen**
asking for:

- **Notes folder** — the folder your notes live in on this computer (created if missing);
- **Sync with a server** — a tick box. Leave it clear and you are done: the app is standalone,
  your notes stay in that folder, and nothing goes on the network. Tick it and it asks for:
  - **Server URL** — e.g. `https://notes.example.org`;
  - **Email / password**, with a "create this account" checkbox for the first account on a new
    server — and, under it, a box to paste an invite link if someone sent you one. Leave email
    and password empty for a `--no-auth` server.

There is no vault to name. With a server the app opens **every vault your account can read**,
one subfolder of the notes folder each, so the tree looks exactly as it does in a browser;
standalone, the vaults are the subfolders that are there, and a first run makes one:

```
~/lemmate/
  Work/          ← a vault named "Work"
  Recipes/
  vault-3f9c2a/  ← a vault nobody has named yet; it takes its name on a later launch
```

The list comes from the server each time the app starts, so a vault created or shared with you
elsewhere appears on the next launch. A vault you create here — *New vault* in the tree — gets
its folder straight away, named after its id until the next launch renames it to the name you
gave it. Folders already on disk open whether or not the server
answers — that is what keeps the app working offline — and a folder you rename yourself keeps
its vault, because the vault is recorded in the folder's `.lemmate/`, not in its name.

Submitting signs in, writes `desktop.toml`, starts the relay and opens the workspace. Every key
has a flag (`--root-dir`, `--server-url`, `--ca-cert`, `--web-dir`, `--config`) and most have an
environment variable; see [`../crates/desktop/README.md`](../crates/desktop/README.md). To open a
single vault folder instead of all of them, pass `--vault-dir` (with `--vault-id` to join an
existing vault) — which is also what a `desktop.toml` written before this keeps doing.

The window is the web client served by the embedded relay, so it keeps working with the server
unreachable — and, standalone, with no server to be unreachable.

**Standalone in full.** Everything in this guide works with no server except what a server is
for: your other devices, other people, sharing and public links, accounts, and the web client
itself. Writing, the tree, tabs and panes, search across every vault, backlinks, tags, outline,
bookmarks, trash, version history, daily notes and templates, attachments, Obsidian import and
pandoc export are all answered by the relay on your machine, out of each vault's `.lemmate/`
folder. The status line at the foot of the sidebar says `local` rather than `online`, and the
sharing commands are not offered.

**Changed your mind later?** Open the command palette (`Ctrl+Shift+P`) and run **Connect a
server…**. Give it the URL and, if the server has accounts, an email and password — with the
same "create this account" box and invite field as the setup screen, and a private-CA path if
you use one. It signs in, checks the server answers, writes `desktop.toml` and restarts the app.

Everything you already have goes up on that first sync: every vault becomes a vault on the
server, with its notes, their history and their attachments, and each keeps its own identity. If
something is wrong — a typo in the URL, the wrong password, a CA the app does not trust — the
dialog says so and nothing is written. If the vault happens to have an id somebody else's
account already owns, the server refuses it and the window shows the refusal.

**Two vaults where you wanted one?** Run **Merge a vault into another…** from the palette. Pick
the vault to empty, the vault to fill, and the folder inside it (its name by default; clear the
box to merge at the root). It then shows you exactly what will happen — where each note lands,
which names clashed and were numbered, which attachments come across — before anything moves.

The notes keep their ids, their history and their images, so `[[links]]` and backlinks still
resolve. The vault you emptied is then gone: its folder here, and the vault itself on the
server, because an empty one left there would come back on the next launch. If that vault syncs
with a server you cannot currently reach, the merge is refused rather than half-done.

You can also run the standalone relay without the desktop window, which is handy for trying it:

```sh
lemmate serve --root ~/lemmate --web-dir ui/dist   # prints the loopback URL to open
```

### (c) Sync a folder from the command line

```sh
lemmate login --server https://notes.example.org --email you@example.org --register
lemmate sync  --vault ~/vault --server https://notes.example.org          # keeps running
```

`login` stores a session token in `credentials.toml` in your configuration directory (mode 0600
where the OS supports it); `sync` picks
it up automatically. First run **publishes** the folder as a new vault and prints the id; to
join an existing vault into an empty folder, pass `--vault-id <ULID>`. Add `--once` to sync and
exit. Add `--serve 127.0.0.1:8081 --web-dir ui/dist` to also run the local relay, which serves
the sync socket, the API and the web client on loopback — this is exactly what the desktop app
embeds.

For a private CA, `--ca-cert ca.pem` (or `LEMMATE_CA_CERT`); `--server https://…` implies `wss://`.

### (d) Web client

Open the server's URL and sign in. There is no vault to pick first: **every vault you can read
is already there**, as the roots of the tree, and you work across all of them at once (§3). The
URL hash is the route: `#/v/<vault>/<note>` for the note you are on (and `#/v/<vault>` for a
vault with nothing open), `#/n/<vault>/<note>` for a note shared directly with you, and
`#/s/<token>` for a public read-only link. The first two follow you as you move, so the address
bar is always a link back to what you are reading.

The desktop app is the same workspace: its local relay runs one sync engine per vault, so the
tree has the same roots and the same routes — with every vault available offline (§1b).

The web client installs. "Add to Home Screen" on iOS, or the install button in a
Chromium browser, gives it its own icon and window — and on iOS that also stops Safari
discarding its stored notes after a week of not being opened. Once installed it starts without
a network, and the whole vault comes with it: note contents are fetched quietly in the
background while you are online, so everything in the tree opens on a plane, not just what you
happened to read first. That fetching only happens once installed — open the same site in an
ordinary browser tab and it behaves as before, downloading notes as you read them, so signing in
from someone else's computer does not leave a copy of everything on it. Write and edit freely — new notes and changes are held on the device and
pushed the next time the app opens with a connection, whether or not you reopen the notes
concerned. The status line at the foot of the sidebar says `offline` while you are.

Search keeps working offline too, over the notes on the device — the pane says so while it is,
because the offline index is broader and more roughly ordered than the server's: it matches
inside words, so `invoice` finds `invoices`, which the server would not.

What still needs the server: backlinks, tags, trash, version history, sharing, and attachments
you have not already looked at.

### (e) On a phone or a narrow window

Below about 720px the shell folds down to one column. A bar across the top carries the sidebar
handle (**☰**), the name of the note you are on, the connection dot, and the two things you
would otherwise reach for with a keyboard: **＋** to open or create a note and **⌘** for the
command palette. The sidebar becomes a drawer over the editor — it slides in from the left and
closes again the moment you open a note, and tapping the dimmed editor or pressing Escape
dismisses it.

Only the focused pane is drawn. Split panes are not lost when the window narrows: they keep
their tabs and scroll positions, and they come back as soon as there is room for them again.

Touch has no right-click, so **press and hold** a note or folder for half a second to get the
menu you would otherwise right-click for — rename, move, share, copy path, trash. Holding is
also how you move things: dragging notes between folders is a mouse gesture the browser does
not offer on touch, so use *Rename / move…* instead.

The pane's chrome measures the **pane**, not the window, so it adapts in a split on a big
screen too. As a pane narrows the outline leaves the margin — there is no margin left to put it
in — and narrower still the view switch, the star and the clock fold into the **⋯** menu, which
lists everything they do. Splitting and closing the pane stay on the strip at every width.

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
one-line property summary — click it to edit). List markers follow the level they are on: `•`,
`◦` and `▪` down a bullet list, and `1.`, `a.`, `i.` down an ordered one, keeping the delimiter
the file holds. An ordered item is numbered by its position, as every markdown renderer numbers
it, so a file full of `1.` still reads 1, 2, 3 and a gap left by an item you indented away
closes by itself. A task line shows its checkbox and no bullet.

Recognised by the indexer and handled by pandoc **on export only**, with no editor decoration
today: footnotes, citations, definition lists, superscript/subscript, bracketed spans, header
and link attributes. Tables are indexed and exported, but in the editor they are only styled as
monospace rows — there is no rendered grid or cell-by-cell editing yet. `![[note]]`
transclusion is parsed but shown as a plain link; only attachment embeds render inline.
Callout `collapse="true"` is not implemented.

### Editing behaviour

- **Three views per pane** — live, source, reading (§3, `Ctrl+E`).
- **Checkboxes are clickable** — clicking a rendered box rewrites `[ ]` ↔ `[x]` in the source.
- **Autocomplete**: type `[[` for note paths, `#` for existing tags. The `@` citation, `:::`
  callout and `/` slash menus described in SPEC §8 are not implemented.
- **Front matter opens folded** and the cursor lands after it.
- **`Tab` nests a list item** under the item above it, and `Shift+Tab` brings it back out; the
  item's children come along, and an ordered item is renumbered for the level it lands on.
  Markdown nests by column, so this is not the same as adding spaces: `Tab` puts the marker
  exactly where the item above holds its content. Anywhere else it is the usual indent.
- **On a phone**, where the keyboard has no `Tab`, the same two commands are the ⇤/⇥ buttons in
  the bar above the note.
- Standard CodeMirror editing: undo/redo, find (`Ctrl+F`), bracket matching.
- `.qmd` files are first-class notes with the same editor, links and search.

---

## 3. Organising

**Vaults are the roots of the tree.** Every vault you can read is listed, with its folders
below it, and each row carries a note count plus, on hover, buttons to add a note (＋), rename
the vault (✎) and import an Obsidian vault into it (⇥). *New vault* at the bottom of the tree
makes another one; a vault only reaches the server once you write something in it. A vault's
name lives in its vault doc, so it is the same on every device — until you give it one, the
tree shows a short form of its id.

**Folders are real folders.** The tree mirrors the vault directory, shows a note count per
folder, and remembers which folders you collapsed. Moving, creating and deleting from the tree
by drag-and-drop is not built yet — use *Rename / move* (which takes a full path) or move the
file on disk.

**Files you add yourself.** Copying a `.md` or `.qmd` file into a vault folder is a way of
creating a note, not something the app tolerates: the watcher picks it up about a second later,
the note appears in the tree, and it syncs like any other. The file is rewritten in place to
carry an `id:` in its front matter (§2) — that line appearing is how you know it was adopted.
The extension decides what it becomes: `.md` and `.qmd` are notes, anything else is treated as
an attachment. Files that arrive while the app is closed are picked up at the next start, so
`git checkout`, `rsync` and an editor writing a new file all work; a file that already carries
an `id:` keeps it, which is what makes moving a note between vault folders by hand a move
rather than a copy.

**Two file browsers.** The toolbar above the tree switches between them, and remembers which
one you left it on:

- **Single tree** — every vault as a root, folders and notes interleaved beneath it.
- **Folders and notes** — folders on top, the selected folder's notes in a list below, like
  Obsidian's *File Tree Alternative*. Clicking a folder selects it; clicking the one you are
  already on folds it. The ↳ button in the list header decides whether the list stops at that
  folder or reaches into its subfolders — with subfolders included, each row says which one it
  came from. Drag the divider between the two halves to re-balance them (double-click resets).

The other three toolbar buttons **expand all**, **collapse all**, and **reveal the open note**
— unfolding the path down to it, selecting its folder in the split view, and scrolling it into
sight. Both views share one set of folds, so collapsing in one collapses in the other.

**The sidebar is resizable.** Drag the divider between it and the editor; double-click the
divider to go back to the default width. It is a `separator` you can also focus and nudge with
the arrow keys. The width is remembered per device, like the pane layout.

**Selecting notes.** A click selects a note and opens it. **Ctrl/Cmd-click** adds one to the
selection without opening it, **Shift-click** takes everything between it and the last one you
touched, in the order the rows are drawn. The toolbar counts what you have picked; clicking a
single note starts over. Both browsers select the same way.

**Moving notes and folders.** Drag a note — or a whole selection, or a folder with everything
under it — onto any folder or vault row. The row you are over lights up, and rows that would
not be a move (a folder into itself, into its own subtree, or back where it already is) simply
do not. A right-click menu on any row offers the same moves by name, plus *New note here*,
*Rename / move*, *Copy path*, *Copy wikilink*, *Bookmark*, *Share* and *Move to trash*; with
several notes selected it acts on all of them.

Inside one vault a move is a rename, so `[[links]]` to the note are rewritten (§2). **Between
vaults it is not**, and Lemmate asks before doing it: a note id belongs to the vault doc that
holds it, so the note is re-created in the target vault with a **new id**, the attachments it
references are copied across, and the original goes to trash. Links to it from notes left
behind in the old vault will not follow it.

**The palette** (`Ctrl+K`, `Ctrl+O`, `Ctrl+P`, `Ctrl+N`) is the one place to search from. It
matches, together and ranked against each other:

- **note titles and paths**, across every vault, each hit labelled with the vault it comes
  from; substring matches rank above subsequence matches;
- **folder names** — choosing one reveals and selects it in the file tree;
- **full text**, from the same FTS index the old search pane used;
- **commands**, with their shortcuts.

Titles outrank folders outrank commands, and body matches come last: matching a note's name is
a stronger signal than matching a word inside it. If nothing matches exactly, the last entry
offers to **create** the note at that path (`.md` appended unless you typed `.md`/`.qmd`) in
the vault you are currently in.

`Enter` opens, `Ctrl+Enter` opens in a split, and `Shift+Enter` creates a note from what you
typed whatever row is highlighted. Typing `>` first narrows to commands only — which is what
`Ctrl+Shift+P` opens with, so it still behaves like a command palette. Shortcut remapping is
not implemented.

**Three ways to look at a note.** The switch in the note header — or `Ctrl+E`, which steps
through them — picks one:

- **Live** (the default) hides markup and renders it in place, showing it again on the line
  your cursor is on. This is the normal editing view.
- **Source** is the markdown itself in a monospace face, nothing hidden and nothing rendered.
- **Reading** renders everything and takes the keyboard away, so you cannot edit by accident.
  Other people's edits still arrive live.

The mode belongs to the **pane**, not the note: split with `Ctrl+\` and you can read a note in
one pane while editing its source in the other. It is saved with the layout, per device.

**One sidebar, and the note itself.** The sidebar is about *finding* a note. Everything *about*
the note you are reading is on the note's own page rather than in a panel beside it.

**Left sidebar**: Files, Tags, Starred. Searching is not a tab here — that is the palette.

- **Files** lists folders and their notes. Rows carry the date they last changed, and the list
  header switches between **Recent** and **Name** order. Where the server or relay cannot
  answer with a listing, rows show no date and the order falls back to the alphabet.
- **Tags** shows every tag with a count; clicking one lists its notes, including nested tags
  under it (`#projects` matches `#projects/alpha`). A tag chip at the foot of a note picks one
  here too, and the choice survives switching to another tab and back.
- **Starred** are bookmarks. They live in the vault doc, so they follow you to every device;
  `Ctrl+Shift+B` toggles one for the current note.
- **Your own name** sits at the very bottom, above the status line, and opens the account menu
  — settings, and the way out (§4).
- **Tags, version history and trash** are per vault, so they show the vault of the note you
  are on. Tabs and bookmarks are not: a pane can hold notes from two vaults side by side, and
  the bookmarks list shows all of them.

**On the page.** A note's measure leaves an empty column on either side, and its own chrome
lives there and underneath it rather than in a column of its own:

- **The folder trail** sits above the first line. The note's name is the heading below it.
- **The outline** is an index in the left margin: the note's headings, right-aligned against
  the text, click to jump. The section you are reading is marked with a rule beside it as you
  scroll. It skips the note's own title, and it is not drawn in a pane too narrow to have a
  margin (roughly the width of two panes on a laptop).
- **Tags and backlinks** are two shelves at the foot of the page, after the note. The tags are
  the ones the index found — inline `#tags` first, then whatever `tags:` the front matter
  declares — lower-cased, as they appear in the Tags pane and in search. Click one to list
  every note that carries it, in the sidebar's Tags tab. **+** adds one, completing from the
  tags the vault already uses: it is written into the note's `tags:` front matter, joining the
  list in whichever style the note already writes it (`[a, b]`, one `- item` per line, or
  `a, b`), and a note with no front matter gets one. Spaces become hyphens and the name is
  lower-cased, so what you get is what the index would have found.

  **Right-click a chip** (press and hold on a phone) for the rest:

  | | |
  |---|---|
  | *Remove from this note* | Takes it off this note alone — out of the front matter, and out of the sentence, which closes up behind it. |
  | *Rename … everywhere* | Renames it in every note that carries it. Nested tags follow their parent: renaming `#projects` makes `#projects/alpha` into `#newname/alpha`. |
  | *Delete … everywhere* | Takes it off every note that carries it. Nested tags are left where they are — `#projects/alpha` is its own tag. The notes themselves are untouched. |

  Both *everywhere* commands say how many notes they are about before they run, and neither
  touches a `#word` inside a code span or a fenced block: the indexers do not read those as
  tags, so nothing that rewrites tags may rewrite them. There is no undo — a rename back is the
  way back.

  Backlinks match links
  whose target is the note's full path, its path without extension, or its basename. Unlinked
  mentions, outgoing links and context snippets are not built.
- **History** is not here at all: it opens in a pane of its own (below).

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

**One tab, unless you ask for another.** Clicking a note — in the tree, in search, in the
quick switcher, a `[[link]]`, a backlink — opens it **in the tab you are already on**, so
browsing does not pile up tabs to close afterwards. The tab it displaces goes on the reopen
stack, and `Ctrl+Shift+T` brings it back.

Two tabs are never displaced: a **pinned** one, and one already showing that note (you just
switch to it). To open something alongside what you have, use the **＋** on the tab strip for
an empty tab, or right-click a note for *Open in a new tab* / *Open in a new pane*.

**Tabs and panes.** Each pane has its own tab strip and editor. Split right with the ◫ at the
right of the strip or `Ctrl+\` (up to three panes — at the limit the control stays and says
so); the ⨯ beside it closes a pane, and `Ctrl+Alt+←/→` moves focus. Pinned tabs sort first and ignore
`Ctrl+W` (unpin from the palette to close them). `Ctrl+Shift+T` reopens the last closed tab —
the last twenty are remembered. The layout, pins and collapsed folders are stored per vault in
the browser's local storage, so they are per device, and tabs pointing at notes that no longer
exist are dropped once the vault has synced.

**Version history** opens in a pane of its own — the clock on the tab strip, `Ctrl+Shift+R`, or
*Show version history* in the palette. It splits right where there is room and reuses the last
pane where there is not, and asking again goes back to the pane already showing it. Its page is
the log: snapshots for the note — automatic ones (taken every 500 updates or 10 minutes) plus
any you name with *Save version…*. Click one and the page becomes that version, rendered the
way the note is, with the lines it no longer shares with the note marked down its left edge.
*Restore* is applied as one more edit, so nothing in the history is lost.
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

**Inviting someone.** An admin mints a single-use link; the person opening it picks their own
email and password and lands in the app signed in. It works once — a second attempt is refused —
and an invited account is never an admin.

```sh
lemmate invite --server https://notes.example.org                 # prints the link
lemmate invite --server … --expires-days 7                        # optional deadline
lemmate invite --server … --list                                  # unused / expired / used by whom
lemmate invite --server … --revoke ID                             # unused ones only
```

In the browser the same thing is under **Account, password and invites…**, in the menu your own
name opens at the foot of the sidebar — the command palette (Ctrl+Shift+P) has it too. The link
is a credential and is not tied to an email address, so send it the way you would send a
password.

**Signing out.** Same menu, at the bottom. It ends this session only; other devices stay signed
in. The row is not drawn on a standalone vault or a server started with `--no-auth`, where
there is no account to leave.

**Changing a password.** Yours needs the current one; an admin can reset anyone's without it,
which is the only recovery path — a self-hosted server has no mail and there is no reset-by-email
link. Either way every *other* session of that account is signed out, so other devices have to
sign in again.

```sh
lemmate passwd --server https://notes.example.org                 # your own
lemmate passwd --server … --email someone@example.org             # admin reset
```

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
| `Ctrl+K` / `Ctrl+O` / `Ctrl+P` | Search notes, folders, text and commands — toggles |
| `Ctrl+N` | Same palette (type a path, Enter creates) |
| `Ctrl+Shift+P` | The palette, narrowed to commands |
| `Ctrl+Shift+F` | The palette (it covers full text) |
| `Ctrl+Shift+R` | Show version history in a pane |
| `Ctrl+Shift+D` | Today's daily note |
| `Ctrl+Shift+B` | Bookmark / unbookmark this note |
| `Ctrl+E` | Cycle this pane's view: live → source → reading |
| `Ctrl+T` | New (empty) tab |
| `Ctrl+W` | Close tab (no-op on a pinned tab) |
| `Ctrl+Shift+T` | Reopen closed tab |
| `Ctrl+\` | Split right |
| `Ctrl+Alt+→` / `Ctrl+Alt+←` | Focus next / previous pane |

Inside the palette: `↑`/`↓` to move, `Enter` to choose, `Ctrl+Enter` to open in a split,
`Shift+Enter` to create a note from what you typed, `Escape` to close.

Some of these — `Ctrl+T`, `Ctrl+W`, `Ctrl+N`, `Ctrl+Shift+T` — are shortcuts the browser
keeps for itself and a web page cannot intercept. They work in the desktop app; in a browser
tab, use the ＋ button, a tab's ×, and the palette instead.

Commands without a shortcut, reachable from the palette: show Files / Tags / Starred / Outline
/ Links / Version history / Trash, set the view to live / source / reading, Share note…,
Rename / move note, Move note to trash, Pin / unpin tab, Close pane, Switch vault, Sign out.

Inside the editor, CodeMirror's own bindings apply — `Ctrl+F` find, `Ctrl+Z` / `Ctrl+Y` undo
and redo, `Tab` indent, `Ctrl+Space` autocomplete. There is no vim keymap (SPEC §17).

---

## 6. Command line

```
notes <command>
```

| Command | What it does |
|---|---|
| `lemmate login --server URL --email E [--register] [--invite LINK] [--ca-cert F]` | Sign in (or create the account) and save the token to `credentials.toml` in your configuration directory. Password prompted if not given. `--invite` takes the link or the bare token and implies `--register`. |
| `lemmate logout --server URL` | Forget the saved token for that server. |
| `lemmate passwd --server URL [--email E]` | Change your password (prompts for the current one), or reset another account's as an admin. Signs every other session of that account out. |
| `lemmate invite --server URL [--expires-days N] [--list] [--revoke ID] [--json]` | Mint, list, or revoke single-use registration links. Admin only. |
| `lemmate sync --vault DIR --server URL [--vault-id ULID] [--once] [--serve ADDR --web-dir DIR] [--ca-cert F] [--token T]` | Keep a folder in sync; optionally run the local relay and serve the web client. |
| `lemmate index PATH [--json]` | Index one file or a whole vault and print what the engine extracts (title, tags, links). |
| `lemmate search VAULT QUERY [--limit N]` | Full-text search over a vault directory, using a throwaway in-memory index. |
| `lemmate import obsidian SRC --into DIR [--overwrite]` | Import an Obsidian vault (see §8). |
| `lemmate export zip VAULT OUT` | Zip the vault's notes and attachments. No pandoc needed. |
| `lemmate doctor` | Print versions, the SQLite schema version, and whether `pandoc`/`quarto` are on `PATH`. |

`LEMMATE_SERVER`, `LEMMATE_TOKEN`, `LEMMATE_CA_CERT`, `LEMMATE_PASSWORD` and `LEMMATE_WEB_DIR` back the
corresponding flags.

The remote commands from SPEC §13.2 (`lemmate ls|cat|new|edit|mv|rm`, `lemmate daily`,
`lemmate vaults`) and the stdio MCP server (`lemmate mcp`, SPEC §13.3) have landed; the CLI's own
`crates/cli/README.md` documents the MCP tool surface.

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

From the app: **Import an Obsidian vault…** in the command palette, or the ⇥ button on a vault
row in the tree. Pick your Obsidian folder, choose whether it goes into an existing vault or a
new one, and the browser uploads it — in batches, with a progress bar — to the server or, on
the desktop, to the local relay, which writes it straight into your vault folder. The summary
at the end counts the notes, attachments, callouts and embeds it handled. Nothing is
overwritten: a path the vault already holds is skipped, so if an import is interrupted you can
simply run it again, and importing the same folder twice does not duplicate anything.

From the command line, the same conversion over a directory:

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
| `.obsidian/bookmarks.json` | Bookmarks are kept: importing through the app puts them straight into the vault's bookmark list; the CLI writes `.lemmate/bookmarks.import.json` |
| `.obsidian/daily-notes.json` | Translated into `.lemmate/daily.import.json` wherever there is a vault folder (the CLI, and an import into the desktop relay); nothing consumes it yet, so check the daily-note path by hand |
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
`<vault>/.lemmate/local.db` and reconciled when the server returns. In the **browser** the notes
and any unsent edits are kept in IndexedDB, so reloading a disconnected tab is safe and the
changes go up when the connection does — whether or not you reopen the notes concerned. An
installed client (§1d) goes further: it starts with no network at all and holds the whole vault,
not only what you have opened. Backlinks, tags, trash, history and sharing still need the
server.

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
| `<config>/credentials.toml` | Saved session tokens, one per server (mode 0600 on Unix) |
| `<config>/desktop.toml` | Desktop app configuration (`root_dir`, the folder the vault folders live in) |
| `<data-dir>/lemmate.db`, `<data-dir>/attachments/` | Everything on the server |

`<config>` is `~/.config/lemmate` on Linux (or `$XDG_CONFIG_HOME/lemmate`), `~/Library/Application
Support/lemmate` on macOS and `%APPDATA%\lemmate` on Windows; `LEMMATE_CONFIG_DIR` overrides it.

**Resetting a device.** The sidecar is a cache, not your data: stop the client, delete
`<vault>/.lemmate/`, and re-sync. Pass `--vault-id <ULID>` (the id is printed by `lemmate sync
--once`, appears in the vault URL, and is what `desktop.toml` stores) so the folder rejoins the
same vault instead of publishing itself as a new one. Any local-only edit that never reached
the server is lost with the journal, so sync before you delete it. To start completely clean,
delete the vault folder as well and let the engine re-materialise it from the server.

**Login appears to do nothing** over HTTPS: the server needs `--secure-cookies`
(`LEMMATE_SECURE_COOKIES=true`) so the browser stores the session cookie — and must *not* have it
on a plain-HTTP deployment. See [`deploy.md`](deploy.md).
