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

See [`deploy.md`](deploy.md) for Docker, a Caddy reverse proxy, running on a rented server, and
backups. The short version:

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
folder. The status dot at the foot of the sidebar's rail says `local` rather than `online`
(hover it), and the sharing commands are not offered.

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

`login` saves a session token — in the system keychain when there is one (macOS Keychain,
Windows Credential Manager, GNOME Keyring or KWallet), otherwise in `credentials.toml` in your
configuration directory (mode 0600 where the OS supports it) — and `sync` picks it up
automatically. `LEMMATE_KEYCHAIN=0` keeps it in the file. On a server that signs in only through
an identity provider there is no password to give: make an access token in the web client
(**Account → Access tokens**, §4) and `lemmate login --server … --token lmt_…`. First run **publishes** the folder as a new vault and prints the id; to
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
concerned. The status dot at the foot of the sidebar's rail turns amber and says `offline`
while you are.

Search keeps working offline too, over the notes on the device — the pane says so while it is,
because the offline index is broader and more roughly ordered than the server's: it matches
inside words, so `invoice` finds `invoices`, which the server would not.

What still needs the server: backlinks, tags, trash, version history, sharing, and attachments
you have not already looked at.

### (e) On a phone or a narrow window

Below about 720px the shell folds down to one column. A bar across the top carries the sidebar
handle (**☰**), the name of the note you are on, the connection dot, today's daily note, and
the two things you would otherwise reach for with a keyboard: the magnifier for the palette and
**⌘** for its commands. On a phone the palette takes the whole screen — the field at the top,
**Cancel** beside it, results down to the keyboard. The sidebar becomes a drawer over the editor,
rail and all — it slides in from the left and closes again the moment you open a note or start
a search, and tapping the dimmed editor or pressing Escape dismisses it.

Only the focused pane is drawn. The others are not lost: they keep their tabs and scroll
positions, and they come back as soon as there is room for them again. While there is more than
one, a chip on the top bar says which you are on (**2/3**); tap it for the list of panes —
note, history or render, by what each is showing — to switch to one, or to close the one you
are on. *Open in a new pane* (long-press a note) works here as it does on a desktop: a pane of
its own, up to three, and after that a tab of its own in the next one — never over the note you
were reading.

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
![[Other Note]]                 note transclusion: the note, drawn in place
![[Other Note#Section]]         …just that section, or #^id for one marked block
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
task checkboxes, blockquotes, callout blocks, and front matter (folded to a one-line property
summary — click it to edit). **Tables** are drawn as tables, with column alignment and the
inline markup above inside cells; click a cell to edit its row's markdown, with the caret in
that cell. Inside a table a wikilink's label needs its pipe escaped, `[[Note\|label]]`, or the
pipe ends the cell. **Fenced code blocks** fold their fences away — the opening one to the
language's name — and are syntax-highlighted in whatever language the fence names (`rust`,
Quarto's `{python}`, Pandoc's `{.haskell}`); a language's grammar is fetched the first time a
block uses it, so offline a never-seen language shows as plain monospace. List markers follow the level they are on: `•`,
`◦` and `▪` down a bullet list, and `1.`, `a.`, `i.` down an ordered one, keeping the delimiter
the file holds. An ordered item is numbered by its position, as every markdown renderer numbers
it, so a file full of `1.` still reads 1, 2, 3 and a gap left by an item you indented away
closes by itself. A task line shows its checkbox and no bullet.

**Note embeds** — `![[Other Note]]` on a line of its own — draw that note in place, in a frame
captioned with its name: read-only, rendered as reading mode renders it, and live, so an edit
to the other note (from any window, or to its file) shows up here as it happens. Front matter
is left out. `![[Other Note#Section]]` shows only that heading's section, down to the next
heading at its level or above; `![[Other Note#^id]]` shows the paragraph or list item that
ends in `^id` (a `^id` on a line of its own marks the table, quote or list above it). Click
the caption to open the note, or anywhere else on the frame to edit the embed's own markdown.
An embed in the middle of a sentence stays a link, as does one naming a note that does not
exist, the note it sits in, or a note already being shown around it — so a note that embeds
itself, or two that embed each other, stop at a link — and embeds nest three deep at most.
The `^id` markers themselves are hidden like any other markup, and come back on the cursor's
line; one on a line of its own needs a blank line between it and a table, or the table takes
it for another row.

Recognised by the indexer and handled by pandoc **on export only**, with no editor decoration
today: footnotes, citations, definition lists, superscript/subscript, bracketed spans, header
and link attributes.
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
- **Formatting without the markdown.** Select some text and a small bar appears over it:
  **bold**, *italic*, ~~strikethrough~~, `code`, a link, and a `[[link]]` to a note. Each
  button toggles, so pressing it again takes the markers off. The same commands have keys —
  `Ctrl+B`, `Ctrl+I`, `Ctrl+Shift+X`, `Ctrl+Shift+K` for a link — and all they ever do is add
  or remove the marker characters in the text.
- **Right-click in a note** for the same commands, plus *Checklist item* (a box on the line, or
  ticks one already there) and *Insert image or file…*, which uploads from your computer and
  puts the embed where you clicked. `Shift`+right-click gets you the browser's own menu, which
  is where its spelling suggestions are.
- **On a phone** the bar above the note holds the buttons the keyboard lacks: ⇤/⇥ for the two
  `Tab` commands, a checklist box, bold, italic, a note link, a link, and an upload (which can
  also take a photo). It scrolls sideways if the screen is narrow. There is no floating bar on
  a touch screen: the phone's own selection menu is already there.
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

**Two file browsers.** The two buttons beside the *Files* heading switch between them, and
remember which one you left it on:

- **Single tree** — every vault as a root, folders and notes interleaved beneath it.
- **Folders and notes** — folders on top, the selected folder's notes in a list below, like
  Obsidian's *File Tree Alternative*. Clicking a folder selects it; clicking the one you are
  already on folds it. The ↳ button in the list header decides whether the list stops at that
  folder or reaches into its subfolders — with subfolders included, each row says which one it
  came from. The note-with-a-plus at the end of that header makes a **new note in the folder**
  the list is showing (or at the vault's root, when the vault itself is selected). Drag the
  divider between the two halves to re-balance them (double-click resets).

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

**Left sidebar**: a rail of icons down its left edge, and the view it picks beside it. From the
top: **search** (the palette, `Ctrl+K`), then the views — **Files**, **Tags**, **Starred**,
**Attachments**, **Trash** — then **today's daily note** and **new note** (in the vault you are
on). At the foot, the status dot and, when you are signed in, your initial, which opens the
account menu. Searching is not a view here — that is the palette.

- **Files** lists folders and their notes. Rows carry the date they last changed, and the list
  header switches between **Recent** and **Name** order. Where the server or relay cannot
  answer with a listing, rows show no date and the order falls back to the alphabet. The two
  buttons beside its heading are the **single tree** and **folders and notes**.
- **Tags** is a tree, because tags are one: `#projects/alpha` sits under `#projects`, named by
  its last segment alone, and folds like a folder does. A branch point is drawn even where
  nothing is tagged with it — a vault can use `#projects/alpha` and never `#projects`. Clicking
  a row lists its notes *and* everything under it, and the count beside it is that same
  number — the notes you will see, counted once each even where a note sits under a branch
  twice. Which branches you fold is remembered per vault, and **Expand all** / **Collapse all**
  sit on the strip above — drawn only where there is a branch to fold. A tag chip at the foot
  of a note picks one here too, opening the branches above it, and the choice survives
  switching to another tab and back. **Right-click a row** (press and hold on a phone) for
  *Rename … everywhere* and *Delete … everywhere*, the same two commands the chips offer and
  with the same meaning; what the tree has no version of is *Remove from this note*, because
  here there is no note in question.
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
  every note that carries it, in the sidebar's Tags view. **+** adds one, completing from the
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

**Daily notes.** `Ctrl+Shift+D`, or the calendar on the sidebar's rail (in the top bar on
a phone), opens today's note — `Daily/YYYY-MM-DD.md` unless the vault says otherwise — creating
it from the daily template if it does not exist yet. It goes in the vault of the note you are
on. `Alt+[` and `Alt+]` step to the previous and next daily note that exists.

**Right-click** the calendar button (or run *Daily notes calendar and settings…*) for a month
view: days that have a note carry a dot, and clicking a day opens its note, creating it if need
be. **Settings** there are per vault and shared by everyone in it:

| Setting | Default | |
|---|---|---|
| Folder | `Daily` | `/` puts daily notes at the vault root |
| File name format | `YYYY-MM-DD` | Moment.js tokens, as in Obsidian — `DD.MM.YYYY`, `YYYY/MM/YYYY-MM-DD dddd`, `gggg-[W]ww`; a `/` makes folders |
| Template | `Templates/Daily.md` | any note in the vault |

Changing them affects new daily notes only; existing ones stay where they are. `lemmate daily`
and the `/daily/{date}` API file days by the same settings.

**Templates.** Put `Templates/Note.md` and a daily template (`Templates/Daily.md` unless the
vault's daily settings name another) in the vault; they are applied when a note or a daily note
is created (the template's own front matter is stripped first). Variables:

| Variable | Expands to |
|---|---|
| `{{date}}` | `YYYY-MM-DD` today — or, in a daily note, that note's day |
| `{{date:FORMAT}}` | the same date in a Moment.js format (`dddd D MMMM`), plus `HH` and `mm` for the time |
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

**Managing files.** The paper-clip on the sidebar's rail lists every file in your vaults that is not
a note — images, stylesheets, `_quarto.yml`, bibliographies, PDFs — as a tree of the folders
that hold them, so a `Slides/` folder with one sub-folder per deck stays tidy. A folder shows
how many files are inside; a file shows how many notes use it, or *vault* for the vault-wide
ones, *kept* for one you put there yourself that nothing uses yet. The chips filter to images,
styles or everything else; the target button opens the folders holding the files of the note
you are reading.

- **Open** a file by clicking it. Text files — `.scss`, `.css`, `.yml`, `.bib`, `.lua`, `.json`
  and friends — open in an editor; **Save** (or `Ctrl+S`) writes the whole file for everyone.
  Files are not edited live the way notes are: if someone else saved it after you opened it,
  Save stops and asks whether to save yours over theirs or load theirs. Images open as a
  preview; anything else offers a download. **Replace…** puts a new version in place.
- **Rename / move…** takes any path in the vault, and rewrites the notes that use the file —
  their links, embeds and front matter — to point at the new one.
- **Delete…** removes the file for everyone, and tells you first which notes still use it.
- **Upload** from the toolbar, from a folder's upload button (hover it, or right-click), or
  from the *Files* shelf at the foot of a note. You choose where the files go: next to the open
  note, in `attachments/`, or in any folder. If a name is taken, choose per file whether to
  replace that file or keep both (the new one gets a `-2` name). Uploaded files are **kept** even
  while no note uses them — upload a theme first, name it in the front matter after.

Each note's page lists the files it uses under **Files**, above its tags — images, a theme
and the partials that theme imports — and its **+ Upload** asks only whether they go next to
the note or in `attachments/`.

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
`Ctrl+W` (unpin them — right-click the tab, or the palette — to close them). `Ctrl+Shift+T` reopens the last closed tab —
the last twenty are remembered. The layout, pins and collapsed folders are stored per vault in
the browser's local storage, so they are per device, and tabs pointing at notes that no longer
exist are dropped once the vault has synced.

**Drag tabs** to rearrange them: along the strip to reorder (pinned tabs stay ahead of the rest),
onto another pane's strip or page to move them there, or onto the left or right third of a page
to split that pane with the tab in the new half — the part of the page that will change lights
up while you hold it there. A pane you drag the last tab out of closes. The same works **between
windows** — the main one and any you moved a note into, or two browser windows on the same site:
drop a tab on another window's strip or page and it moves there, and a window that loses its last
tab this way closes. A window that does not hold that note (still loading, or signed in as someone
else) refuses the drop, and the tab stays where it was. Let a tab go **outside every window** and
it opens in a new one of its own, about where you let go (in a browser, if it blocks the popup,
the tab stays put).

**A note in its own window.** Right-click a tab, or open `⋯`, and choose *Move to new window*
(*Move tab to new window* in the palette). The tab leaves its pane and the note opens in a window
with no sidebar — another app window in the desktop app, a popup in a browser. Links, the palette
and the other tabs you open there all work as usual; the window just keeps no layout of its own,
so closing it loses nothing and the main window's panes are restored as you left them. The window
closes by itself when its last tab does. Not offered
on a phone, where there is no second window to put it in.

**Version history** opens in a pane of its own — the clock on the tab strip, `Ctrl+Shift+R`, or
*Show version history* in the palette. It splits right where there is room and reuses the last
pane where there is not, and asking again goes back to the pane already showing it. Its page is
the log: snapshots for the note — automatic ones (taken every 500 updates or 10 minutes) plus
any you name with *Save version…*. Click one and the page becomes that version, rendered the
way the note is, with the lines it no longer shares with the note marked down its left edge.
*Restore* is applied as one more edit, so nothing in the history is lost.
Snapshots are kept forever; the raw update log behind them is pruned after `--retain-days`
(90 by default).

**Trash.** *Move to trash* removes the note from the vault doc; the file disappears from every
synced replica and the tab closes everywhere. The note's update log and versions stay in the
store, so nothing is destroyed yet. The trash view — the trash-can on the sidebar's rail, or
*Show trash* in the palette — lists a vault's deleted notes,
newest first, starting with the vault of the note you are on (a picker switches vaults when
you have more than one). The folder on the rail goes back to the files. **Restore**
puts a note back at its old path, or at `… (restored).md` if something has taken that path
since, and opens it; on a synced vault its file comes back on every replica.

The server forgets a note for good once it has been in the trash longer than
`--attachment-grace-days` (30 by default), and purges attachments no note references any more
after the same period; referencing one again before then rescues it. A standalone vault
(`lemmate serve`, or the desktop app with no server) never purges: its trash keeps everything.

---

## 4. Collaboration

**Accounts.** Email + password, or an OpenID Connect provider (Authelia, Keycloak, Google, …) —
or both. Sessions are opaque bearer tokens (hashed at rest), sent as `Authorization: Bearer …`
by native clients and as an HttpOnly cookie by the browser.

**Signing in with an identity provider.** With OIDC set up (see `docs/deploy.md`), the sign-in
page shows **Sign in with <provider>**. The first time an identity signs in:

- if an account has the same email *and the provider says the address is verified*, the identity
  is tied to that account — which is how password accounts move over;
- otherwise an account is created when anyone could register (an empty server, whose first
  account is the admin, or `--allow-registration`), or when the sign-in started from an invite
  link;
- otherwise it is refused, with the reason on the sign-in page.

After that the identity always signs into the same account, whatever its email becomes. A
server can turn passwords off entirely (`--disable-password-login`); the password fields, the
password change and password registration then disappear, and invites create accounts through
the provider.

**Access tokens** are for the CLI, scripts, MCP and the desktop app, and are the way to reach a
server with passwords turned off from any of them. Make one under **Account, password, tokens and
invites…**: give it a name, optionally tick the vaults it may reach, optionally make it read
only, optionally let it expire. The token (`lmt_…`) is shown once. A token never carries admin
rights and cannot make, list or revoke tokens or change the password; changing your password does
not revoke tokens — revoke them in the same dialog, which also says when each was last used.

```sh
lemmate login --server https://notes.example.org --token lmt_…     # saves it like a session
curl -H "Authorization: Bearer lmt_…" https://notes.example.org/api/v1/vaults
```

**Inviting someone.** An admin mints a single-use link; the person opening it picks their own
email and password and lands in the app signed in. It works once — a second attempt is refused —
and an invited account is never an admin.

```sh
lemmate invite --server https://notes.example.org                 # prints the link
lemmate invite --server … --expires-days 7                        # optional deadline
lemmate invite --server … --list                                  # unused / expired / used by whom
lemmate invite --server … --revoke ID                             # unused ones only
```

In the browser the same thing is under **Account, password, tokens and invites…**, in the menu your initial
opens at the foot of the sidebar's rail — the command palette (Ctrl+Shift+P) has it too. The link
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
others", and hovering the status dot says how many people are editing. Your display name comes from
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
| `Ctrl+Shift+D` | Today's daily note (right-click its button for the calendar) |
| `Alt+[` / `Alt+]` | Previous / next daily note |
| `Ctrl+Shift+B` | Bookmark / unbookmark this note |
| `Ctrl+E` | Cycle this pane's view: live → source → reading |
| `Ctrl+T` | New (empty) tab |
| `Ctrl+W` | Close tab (no-op on a pinned tab) |
| `Ctrl+Shift+T` | Reopen closed tab |
| `Ctrl+\` | Split right |
| `Ctrl+Alt+→` / `Ctrl+Alt+←` | Focus next / previous pane |
| `Ctrl+B` / `Ctrl+I` | Bold / italic, in a note — toggles |
| `Ctrl+Shift+X` | Strikethrough, in a note — toggles |
| `Ctrl+Shift+K` | Link: the selection becomes its text (or its target, if it is a URL) |

Inside the palette: `↑`/`↓` to move, `Enter` to choose, `Ctrl+Enter` to open in a split,
`Shift+Enter` to create a note from what you typed, `Escape` to close.

Some of these — `Ctrl+T`, `Ctrl+W`, `Ctrl+N`, `Ctrl+Shift+T` — are shortcuts the browser
keeps for itself and a web page cannot intercept. They work in the desktop app; in a browser
tab, use the ＋ button, a tab's ×, and the palette instead.

Commands without a shortcut, reachable from the palette: show Files / Tags / Starred / Outline
/ Links / Version history / Trash, set the view to live / source / reading, Share note…,
Rename / move note, Move note to trash, Pin / unpin tab, Close pane, Switch vault, Sign out.

Inside the editor, CodeMirror's own bindings apply — `Ctrl+F` find, `Ctrl+Z` / `Ctrl+Y` undo
and redo, `Tab` indent, `Ctrl+Space` autocomplete — except `Ctrl+I`, which is italic here
rather than CodeMirror's "select the enclosing syntax". There is no vim keymap (SPEC §17).

---

## 6. Command line

```
notes <command>
```

| Command | What it does |
|---|---|
| `lemmate login --server URL --email E [--register] [--invite LINK] [--ca-cert F]` | Sign in (or create the account) and save the token — in the system keychain, or `credentials.toml` in your configuration directory where there is none. Password prompted if not given. `--invite` takes the link or the bare token and implies `--register`. |
| `lemmate login --server URL --token lmt_…` | Save an access token made in the web client instead (checked against the server first): the way in when the server signs in only through an identity provider. |
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

**Citations.** `[@key]` is resolved (`--citeproc`) against the note's own bibliography when its
front matter names one — `bibliography: refs.bib`, or a list, relative to the note as in Quarto
(a leading `/` is the vault root) — and otherwise against the vault's `export/references.bib`.
The style is the note's `csl:`, else `export/style.csl`, else pandoc's default (Chicago
author-date). Only files in the vault count, so a note cannot point an export at other files on
the host. This works on the server and on the local relay alike.

`export/defaults.yaml` (passed as `--defaults`) and image resource paths apply only where the
exporter has the vault folder — the local relay (desktop app, `lemmate serve`); server exports
render the note text with image links left relative.

### Rendering with Quarto

**Render with Quarto** — in the command palette, in a note's `···` menu, or the page icon on
the strip of a `.qmd` note — renders the note **the way its front matter says**: the first
format it declares that a render can make. A deck (`format: revealjs`) or a page (`html`) opens
in a pane beside the note; a `pdf` (or `typst`) or `docx` note is saved to your downloads, and
the pane says so. The picker on the pane's bar renders it another way — as a plain page, as
slides, or as a PDF or Word file to download — whatever the note declares. **Full screen** (the four corners)
fills the screen with the render — the browser's own full screen where it has one, the whole
app window on an iPhone — and stays on the slide you were on; the **×** in its corner (or Esc)
brings the pane back. **Open outside** (the box with an arrow) opens the same render in a
browser tab of its own — in the desktop app, in your default browser; in the app installed on
an iPhone, long-press it for *Open in* your browser — straight away. A browser that is not
signed in goes through the sign-in first and comes back to the render: the page the pane already has is kept for half an hour, and only rendered again after.
On a phone, swipe sideways to turn a deck's slides.

**Speaker view.** Press **S** in a rendered deck (or pick *Speaker View* from its menu) for
Lemmate's speaker view, in a window of its own: the current slide, the next one, the slide's
notes (`::: {.notes}`), the slide count, a timer and the clock. Its **Previous**/**Next** and
arrow keys turn the deck in the other window, which is the one to put on the projector — full
screen from the pane, or opened in a tab of its own. It is not reveal.js's own speaker view,
which cannot work in a deck that runs sandboxed; this one does the same job by messages. In
the desktop app, open the deck in your browser first (the arrow icon) and press **S** there.

A render opens in a pane beside the note, and later ones gather there as **tabs**: rendering a
second note adds a tab to the pane showing a render rather than opening another pane, and
rendering one that is already open just brings its tab forward. A rendered tab is otherwise a tab
like any other — drag it along the strip, into any pane (a note's included) or against a pane's
edge to split; right-click it to pin it or *Move to new window*, or drag it out of the window.
Each tab keeps its render, so switching away and back, or moving the tab to another pane, does
not run Quarto again (a move reloads the page, though, so a deck starts from its first slide). Quarto's
page runs in a sandboxed frame: its own scripts and styles work, and nothing in it can reach the
app or your session. Links to other sites open in a new tab. A render takes a few seconds, so
it runs when you ask: an edit afterwards marks the pane *Changed since this render*, and
**Re-render** brings it up to date. If Quarto refuses the note, the pane shows its message.

PDFs are made through the Typst that Quarto bundles — no LaTeX needed — so a note's `pdf:`
options that only LaTeX understands (a `documentclass`, say) do not apply; Typst's do. The
palette's **Render with Quarto as PDF / Word document / slides** save a file directly.

What goes in:

- **The note's front matter, as Quarto reads it** — `title`, `author`, `format:` options,
  `toc`, and so on. That is what makes it a Quarto render rather than an export.
- **No code runs.** Every render passes `--no-execute`: a `{python}` or `{r}` cell is shown
  with its source, never executed, whatever the front matter says.
- **Images come along.** The note is rendered at its own path with the attachments it
  references laid out around it, so `![](../attachments/x.png)` and `![[x.png]]` both resolve,
  and an Obsidian width (`![[x.png|300]]`) is kept. HTML and slides embed them.
- **Wikilinks become their labels.** Another note is not part of the rendered document, so
  `[[Plan|the plan]]` renders as *the plan* (marked `.wikilink` for a stylesheet to find).
- **Companion files come along too.** Any file the front matter names — `theme: [cosmo,
  custom.scss]`, `css:`, `include-in-header:`, `filters: [wordcount.lua]`, `reference-doc:`,
  `bibliography:` — relative to the note, the vault root, or `attachments/`, and whatever a
  stylesheet `@import`s, `@use`s or `@forward`s in turn (`'vars'` finds `_vars.scss`).
- **The vault's own Quarto settings.** A `_quarto.yml` at the vault root is the base for every
  render, so a theme shared across documents goes there once; `_metadata.yml` files apply to
  their folder as Quarto has them do. Three things are settled whatever it says: the project
  is the one note (its `project:` — website, book, output directory, `pre-render` and
  `post-render` scripts — is dropped), HTML and slides are self-contained, and a note's front
  matter still wins over it.
- **The vault's bibliography** — `export/references.bib`, and `export/style.csl` — is used
  unless `_quarto.yml` or the note's front matter names its own `bibliography:`.

These files are ordinary attachments as far as sync goes: a file a note depends on this way is
recorded in the vault and synced like an image it embeds, and so are `_quarto.yml`, every
`_metadata.yml` and everything under `export/`, whether or not a note mentions them. A
stylesheet nothing depends on stays local. Editing a theme to import a new partial picks the
partial up without touching the notes that use it. Order does not matter either: a file that
turns up after the note naming it — the image copied in once the link is written, the theme
saved after the front matter names it — is found as it arrives, or at the next start of the
desktop app if it was copied in while the app was closed.

`quarto` is found through `--quarto PATH` / `LEMMATE_QUARTO` on the server, `LEMMATE_QUARTO` for
the desktop app and `lemmate serve`, and `PATH` otherwise; without one, rendering answers
**501** and the pane says so. The Docker image includes it. `POST
/api/v1/vaults/{vault}/notes/{id}/render` with `{"format": "html" | "pdf" | "docx" |
"revealjs"}` is the endpoint behind all of it; a render Quarto rejects answers **422** with its
message.

**On a shared server**, a note's front matter can name Lua filters and files to include, and
those run and are read on the server when the note is rendered — by anyone who can edit a note.
In a container that reaches only the container, but if that is still more than you want, set
`--disable-quarto` / `LEMMATE_DISABLE_QUARTO=true` and renders answer 501.

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
| `.obsidian/bookmarks.json` | Bookmarks are kept: importing through the app puts them straight into the vault's bookmark list; the CLI writes `.lemmate/bookmarks.import.json`, which the next `lemmate sync` or desktop launch moves into the list |
| `.obsidian/daily-notes.json` | Becomes the vault's daily-note settings — folder, date format and template, so days keep the names Obsidian gave them (a vault with no folder set keeps them at the root). Through the app they apply at once; the CLI writes `.lemmate/daily.import.json`, adopted on the next sync like the bookmarks |
| `.obsidian/`, `.trash/` | Skipped |

Note ids are not written during import — the sync engine assigns them on first sync.

Deliberately absent, and not planned (SPEC §15): plugins and in-app scripting, graph view,
canvas, kanban, Dataview-style queries, spaced repetition, WYSIWYG rich text, end-to-end
encryption, and peer-to-peer sync.

---

## 9. Troubleshooting

**The status dot.** At the foot of the sidebar's rail: green when the socket is `online`, amber
when `connecting` or `offline`. Hover it for the words, the note count and, while the vault doc
is still catching up, "syncing…". Reconnection is automatic with exponential backoff up to 30 s.

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
updates on one you can only view. The window says so in a red strip above the status line —
"Permission denied by the server … your last change was not saved" — with **Reload** and
**Dismiss**. Check your role on the vault (`GET /api/v1/vaults/{v}/members`): a viewer cannot
write. On the desktop the refusal comes through the local relay, and a window opened after it
is told too.

**Where things live.**

| Path | What |
|---|---|
| `<vault>/.lemmate/local.db` | Local update log, snapshots, index, and the vault id for this folder |
| `<vault>/.lemmate/attachments/` | Content-addressed attachment cache |
| `<vault>/attachments/` | The human-readable projection of referenced attachments |
| `<config>/credentials.toml` | Saved session tokens, one per server (mode 0600 on Unix) — or, when the token is in the system keychain (service `lemmate`), just a note saying so |
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
