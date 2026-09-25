<script lang="ts">
  import { onDestroy, untrack } from 'svelte'
  import { api, authState, type RenderFormat, type User } from './lib/api.ts'
  import Login from './components/Login.svelte'
  import AccountDialog from './components/AccountDialog.svelte'
  import ContextMenu, { menuAt, type MenuState } from './components/ContextMenu.svelte'
  import { cleanTag, removeTagFromText, renameTagInText } from './lib/tagedit.ts'
  import Setup from './components/Setup.svelte'
  import ConnectServer from './components/ConnectServer.svelte'
  import MergeVaults from './components/MergeVaults.svelte'
  import DailyDialog from './components/DailyDialog.svelte'
  import { compare as compareDays, dayOf, formatDate, iso, pathFor, templateOf, today, type Day } from './lib/daily.ts'
  import { VaultSession, displayName } from './lib/vault.svelte.ts'
  import { notePath, unnamedNote } from './lib/notename.ts'
  import { Workspace } from './lib/workspace.svelte.ts'
  import { ulid } from './lib/ulid.ts'
  import FilesPane from './components/FilesPane.svelte'
  import type { VaultNode } from './lib/tree.ts'
  import { plan, type DragPayload } from './lib/moves.ts'
  import type { ViewMode } from './lib/editor/setup.ts'
  import { clamp, dragResize } from './lib/resize.ts'
  import { media, NARROW } from './lib/media.svelte.ts'
  import Pane, { isBlank, type PaneState } from './components/Pane.svelte'
  import { inNewPane, moveTab, notePane, removeTab, type TabDrag, type TabDrop } from './lib/tabmoves.ts'

  import SearchPane from './components/SearchPane.svelte'
  import TagsPane from './components/TagsPane.svelte'
  import Palette, { type Command } from './components/Palette.svelte'
  import TrashPane from './components/TrashPane.svelte'
  import AttachmentsPane from './components/AttachmentsPane.svelte'
  import UploadDialog from './components/UploadDialog.svelte'
  import { fileTab, isFileTab, parseFileTab } from './lib/filetabs.ts'
  import { isRenderTab, renderTab, tabNote } from './lib/rendertabs.ts'
  import { appAuthorize, renderReturn } from './lib/next.ts'
  import AuthorizeApp from './components/AuthorizeApp.svelte'
  import type { FileEntry } from './lib/api.ts'
  import ShareDialog from './components/ShareDialog.svelte'
  import SharedView from './components/SharedView.svelte'
  import ImportDialog from './components/ImportDialog.svelte'
  import type { SharedNote } from './lib/api.ts'
  import Modal from './components/Modal.svelte'
  import Icon, { type IconName } from './components/Icon.svelte'

  // ---- first run (desktop): the relay serves the UI in setup mode until configured
  let setup = $state<{ config_path: string; suggested_root_dir: string } | null>(null)
  let setupStarting = $state(false)
  /** A standalone app (SPEC §3.2): the relay behind this page has no server, so there is no
   *  connection to be online with and nobody else's changes to wait for. */
  let localOnly = $state(false)
  /** Served by a local relay at all (standalone or syncing). Sharing, members and invites are
   *  the server's to answer and the relay has no route for them, so they are not offered here;
   *  the same account reaches them through the web client. */
  let onRelay = $state(false)
  /** A standalone shell that can write a server into its own configuration (SPEC §3.2) — the
   *  desktop app, but not `lemmate serve`, which is configured by flags. */
  let canConnect = $state(false)
  let configPath = $state('')
  let connectOpen = $state(false)
  let mergeOpen = $state(false)
  if (!readPublicToken())
    fetch('/api/v1/local/setup')
      .then((r) => (r.ok ? r.json() : null))
      .then(
        (
          j: {
            configured?: boolean
            mode?: string
            server?: string | null
            can_connect?: boolean
            config_path?: string
            suggested_root_dir?: string
          } | null,
        ) => {
          if (j && j.configured === false) setup = { config_path: j.config_path ?? '', suggested_root_dir: j.suggested_root_dir ?? '' }
          // Only a configured relay answers with a mode; a server has no such route.
          onRelay = typeof j?.mode === 'string'
          localOnly = j?.mode === 'local'
          canConnect = j?.can_connect === true
          configPath = j?.config_path ?? ''
        },
      )
      .catch(() => {})

  /** A native app asking to be signed in: this page is only the approval, never the workspace. */
  const appAuth = appAuthorize(location.search)

  // ---- account: the API answers 401 until signed in (never with --no-auth or the relay)
  let authRequired = $state(false)
  let me = $state<User | null>(null)
  authState.onUnauthorized = () => {
    authRequired = true
  }
  if (!readPublicToken())
    api
      .me()
      .then((u) => (me = u))
      .catch(() => {})
  $effect(() => {
    // The editor labels our cursor for others with this name.
    if (me) (window as unknown as { lemmate?: { userName?: string } }).lemmate = { ...((window as unknown as { lemmate?: object }).lemmate ?? {}), userName: me.display_name }
  })
  async function signedIn() {
    // Signed in on the way to a render (a page opened in a browser that had no session): go on.
    const next = renderReturn(location.search)
    if (next) return void location.replace(next)
    authRequired = false
    // The invite is spent now; leaving it in the URL would only re-show the register form.
    if (invite) {
      invite = null
      location.hash = ''
    }
    me = await api.me().catch(() => null)
    workspace?.refresh()
  }
  /** Signed out here, so the sign-in page must not bounce straight back through the provider. */
  let signedOut = $state(false)
  async function signOut() {
    await api.logout().catch(() => {})
    signedOut = true
    me = null
    location.hash = ''
    authRequired = true
  }

  // ---- the workspace: every vault at once (SPEC §9), on one socket
  //
  // Routes: `#/v/<vault>` focuses a vault, `#/v/<vault>/<note>` a note inside it, and both are
  // written back as you move around. `#/w/<vault>/<note>` is the same note in a window of its
  // own (a tab's *Move to new window*); `render:<note>` in place of the note is its render. `#/n/<vault>/<note>` (a note shared directly with you) and
  // `#/s/<token>` (a public link) are single-note views with no workspace behind them.
  const ULID = '[0-9A-HJKMNP-TV-Z]{26}'
  /**
   * A window a tab was moved out into: the whole workspace behind it, so links, the palette and
   * other vaults all still work, but no sidebar and — above all — no say in the saved layout.
   * Every window on this origin shares one `localStorage`, and a window holding one note must
   * not replace the main window's panes with it. Fixed for the life of the page: the route it
   * writes back keeps the `w`.
   */
  const detached = /^#\/w\//u.test(location.hash)
  let workspace = $state<Workspace | null>(null)
  /** The single-note session behind `#/n/…`; never a workspace member. */
  let solo = $state<VaultSession | null>(null)
  /** Which vault the sidebar and "new note" act on when no note is focused. */
  let focusVault = $state<string | null>(null)
  let routeNote = $state<string | null>(readRouteNote())

  function readRouteVault(): string | null {
    const m = new RegExp(`^#/[vw]/(${ULID})`, 'u').exec(location.hash)
    if (m) return m[1]!
    const n = new RegExp(`^#/n/(${ULID})/(${ULID})`, 'u').exec(location.hash)
    return n ? n[1]! : null
  }
  function readRouteNote(): string | null {
    const m = new RegExp(`^#/[vw]/${ULID}/((?:render:)?${ULID})`, 'u').exec(location.hash)
    return m ? m[1]! : null
  }
  function readNoteOnly(): string | null {
    const n = new RegExp(`^#/n/(${ULID})/(${ULID})`, 'u').exec(location.hash)
    return n ? n[2]! : null
  }
  function readPublicToken(): string | null {
    const m = /^#\/s\/([0-9a-f]{64})/u.exec(location.hash)
    return m ? m[1]! : null
  }
  /** #/invite/<token>: a single-use registration link an admin handed out (SPEC §11.1). */
  function readInvite(): string | null {
    const m = /^#\/invite\/([0-9a-f]{64})/u.exec(location.hash)
    return m ? m[1]! : null
  }
  let invite = $state<string | null>(readInvite())
  let publicToken = $state<string | null>(readPublicToken())
  let noteOnly = $state<string | null>(readNoteOnly())
  window.addEventListener('hashchange', () => {
    publicToken = readPublicToken()
    noteOnly = readNoteOnly()
    invite = readInvite()
    const v = readRouteVault()
    if (v && !noteOnly) focusVault = v
    const n = readRouteNote()
    if (n && n !== activeTab) routeNote = n
  })
  let sharedWithMe: SharedNote[] = $state([])
  let shareOpen = $state(false)
  let accountOpen = $state(false)
  /** Whichever right-click / drop-down menu is open: the account's, or a tag chip's. */
  let menu = $state<MenuState | null>(null)
  let importInto = $state<string | null | undefined>(undefined)
  /** The daily-note calendar, open on this month; null when closed. */
  let calendarAt = $state<Day | null>(null)
  /** Files picked for the upload dialog, and where they were asked to go (SPEC §9). */
  let uploading = $state<{ vault: string; folder: string | null; files: File[] } | null>(null)

  // The single-note view stands alone: one session, one pane, its own socket.
  $effect(() => {
    const only = noteOnly
    const vault = readRouteVault()
    untrack(() => {
      solo?.destroy()
      solo = only && vault ? new VaultSession(vault, { noteOnly: true }) : null
      if (solo && only) {
        panes = [{ id: ++paneSeq, tabs: [only], active: only, mode: 'live' }]
        focusedPane = 0
        pinned = []
        closed = []
      }
    })
  })

  // Everything else runs in the workspace, created once and kept for the session.
  $effect(() => {
    if (publicToken || noteOnly || workspace || appAuth) return
    untrack(() => {
      const ws = new Workspace()
      workspace = ws
      // Debug/automation handle (used by scripts/cdp.mjs smoke runs).
      ;(window as unknown as { lemmate?: unknown }).lemmate = { workspace: ws }
      // A detached window starts empty and opens the note its route names once it is known.
      const restored = detached ? { panes: [blankPane()], focused: 0 } : loadLayout()
      panes = restored.panes
      focusedPane = restored.focused
      pinned = loadPinned()
      focusVault = readRouteVault()
      ws.refresh().then((vaults) => {
        if (!focusVault && vaults.length) focusVault = vaults[0]!.id
      })
      if (!onRelay) api.sharedWithMe().then((s) => (sharedWithMe = s)).catch(() => (sharedWithMe = []))
    })
  })
  onDestroy(() => {
    workspace?.destroy()
    solo?.destroy()
  })

  /** Open a vault that is not on the server yet, or one someone gave you the id of. */
  async function newVault() {
    const ws = workspace
    if (!ws) return
    const name = (await ask({ kind: 'prompt', title: 'New vault', placeholder: 'Name (optional)' }))?.trim()
    if (name === undefined) return
    const session = ws.add(ulid())
    if (name) session.setName(name)
    focusVault = session.id
    palette = ''
  }
  async function renameVault(vault: string) {
    const session = workspace?.get(vault)
    if (!session) return
    const next = await ask({ kind: 'prompt', title: 'Rename vault', initial: session.name })
    if (next !== null) session.setName(next)
  }

  // ---- tabs and panes (SPEC §9)
  const MAX_PANES = 3
  let paneSeq = 0
  let blankSeq = 0
  let panes: PaneState[] = $state([blankPane()])
  let focusedPane = $state(0)
  /** Recently closed note ids, most recent last (Ctrl+Shift+T reopens). */
  let closed: string[] = $state([])
  let pinned: string[] = $state([])
  let presenceByPane: Record<number, string[]> = $state({})
  let layoutRestored = $state(false)

  let sidebar = $state<'files' | 'search' | 'tags' | 'bookmarks'>('files')
  /** The Files tab's third view, beside its two layouts (FilesPane). */
  let trashOpen = $state(false)
  /** The Files tab's attachments view (SPEC §9). */
  let attachmentsOpen = $state(false)
  /** What the rail shows as picked. Attachments and trash are views *of* the Files pane — they
   *  share its toolbar and its vault — so they live there as two flags, and the rail folds the
   *  three back into one choice. */
  type View = 'files' | 'tags' | 'bookmarks' | 'attachments' | 'trash'
  const RAIL: { id: View; label: string; icon: IconName }[] = [
    { id: 'files', label: 'Files', icon: 'files' },
    { id: 'tags', label: 'Tags', icon: 'tags' },
    { id: 'bookmarks', label: 'Starred', icon: 'star' },
    { id: 'attachments', label: 'Attachments — the files that are not notes', icon: 'attach' },
    { id: 'trash', label: 'Trash — deleted notes, and restoring them', icon: 'trash' },
  ]
  let view: View = $derived(
    sidebar === 'tags' || sidebar === 'bookmarks' ? sidebar : trashOpen ? 'trash' : attachmentsOpen ? 'attachments' : 'files',
  )
  function show(v: View) {
    if (v === 'tags' || v === 'bookmarks') {
      sidebar = v
      return
    }
    sidebar = 'files'
    trashOpen = v === 'trash'
    attachmentsOpen = v === 'attachments'
  }
  /** The avatar's letter: the first of the display name, whatever script it is in. */
  function initial(name: string): string {
    return [...name.trim()][0]?.toUpperCase() ?? '?'
  }
  let attachmentsPane: AttachmentsPane | undefined = $state()
  /** The tag the Tags pane is listing. Here rather than in the pane: a tag chip at the foot of
   *  a note picks one too, and the pane is unmounted whenever another tab is showing. */
  let tagFilter: string | null = $state(null)
  /** A tag chip on a note's page: show what else carries it, in the sidebar where tags live. */
  function filterByTag(tag: string) {
    tagFilter = tag
    sidebar = 'tags'
    // On a phone the sidebar is a drawer, and the answer is inside it.
    if (narrow.current) drawer = true
  }

  /**
   * The menu a tag opens, on a chip at the foot of a note and on a row of the Tags tree. Every
   * label says its own scope, because the vault-wide pair reach every note there is and a mass
   * edit that reads as a local one is the way to lose an afternoon's filing.
   *
   * `noteId` is what separates the two places: a chip is on a note, so it can be taken off that
   * note alone, and a row in the tree is about the vault and has no such note to speak of.
   */
  function tagMenu(tag: string, vault: string, e: MouseEvent, noteId?: string) {
    menu = menuAt(e, [
      { label: `Show notes tagged #${tag}`, run: () => filterByTag(tag) },
      { separator: true, label: '' },
      ...(noteId ? [{ label: 'Remove from this note', run: () => void removeTagHere(tag, noteId) }] : []),
      { label: `Rename #${tag} everywhere…`, run: () => void renameTag(tag, vault) },
      { label: `Delete #${tag} everywhere…`, danger: true, run: () => void deleteTag(tag, vault) },
    ])
  }

  /** Off this note only — the everyday case, and the one a `+` puts back. */
  async function removeTagHere(tag: string, noteId: string) {
    const s = sessionOf(noteId)
    if (!s) return
    await s.rewriteNotes([noteId], (text) => removeTagFromText(text, tag))
    tagsVersion++
  }

  /** Every note the vault says carries it — which is what "rename a tag" can only mean. */
  async function taggedNotes(vault: string, tag: string): Promise<string[]> {
    return (await api.tagged(vault, tag).catch(() => [])).map((n) => n.id)
  }

  async function renameTag(tag: string, vault: string) {
    const s = solo ?? workspace?.get(vault)
    if (!s) return
    const ids = await taggedNotes(s.id, tag)
    const typed = await ask({
      kind: 'prompt',
      title: `Rename #${tag}`,
      body: `In ${ids.length} ${ids.length === 1 ? 'note' : 'notes'}, in their text as well as their front matter. Nested tags follow: #${tag}/something becomes the new name's.`,
      initial: tag,
      confirmLabel: 'Rename',
    })
    const next = typed === null ? '' : cleanTag(typed)
    if (!next || next === tag) return
    const changed = await s.rewriteNotes(ids, (text) => renameTagInText(text, tag, next))
    tagsVersion++
    if (tagFilter === tag) tagFilter = next
    if (changed === 0) await ask({ kind: 'confirm', title: `Nothing carried #${tag}.`, confirmLabel: 'OK' })
  }

  async function deleteTag(tag: string, vault: string) {
    const s = solo ?? workspace?.get(vault)
    if (!s) return
    const ids = await taggedNotes(s.id, tag)
    const ok = await ask({
      kind: 'confirm',
      title: `Delete #${tag} everywhere?`,
      body: `It comes off ${ids.length} ${ids.length === 1 ? 'note' : 'notes'}, out of their text as well as their front matter. Nested tags like #${tag}/something are left where they are. The notes themselves are not touched.`,
      confirmLabel: 'Delete',
      danger: true,
    })
    if (ok === null) return
    await s.rewriteNotes(ids, (text) => removeTagFromText(text, tag))
    tagsVersion++
    if (tagFilter === tag) tagFilter = null
  }
  /** The palette, or null when closed; the string is what it opens with — `>` for commands. */
  let palette = $state<string | null>(null)
  let revealFolder: ((vault: string, folder: string) => void) | undefined = $state()
  let tagsVersion = $state(0)

  // ---- narrow shell: a phone has room for the sidebar or a note, not both
  //
  // The sidebar becomes a drawer over the editor, the splitter goes away, and only the focused
  // pane is drawn — the others keep their tabs and scroll positions behind it, so widening the
  // window brings them straight back. Everything here is layout: the panes themselves, the
  // sessions and the sockets are untouched by how many of them are on screen.
  const narrow = media(NARROW)
  let drawer = $state(false)
  // On a phone the palette is a full-screen search, opened from the drawer as often as from the
  // top bar; Cancel should land on the note, not on a drawer left open underneath it.
  $effect(() => {
    if (palette !== null && narrow.current) drawer = false
  })

  // ---- sidebar width: drag the divider, double-click it to go back to the default
  const SIDE_MIN = 180
  const SIDE_MAX = 640
  const SIDE_DEFAULT = 272
  let sideWidth = $state(loadSideWidth())
  function loadSideWidth(): number {
    try {
      const n = Number(localStorage.getItem('lemmate.sidebar.width'))
      if (Number.isFinite(n) && n > 0) return clamp(n, SIDE_MIN, SIDE_MAX)
    } catch {
      /* private mode */
    }
    return SIDE_DEFAULT
  }
  function saveSideWidth(w: number) {
    sideWidth = w
    try {
      localStorage.setItem('lemmate.sidebar.width', String(w))
    } catch {
      /* private mode */
    }
  }

  function blankPane(): PaneState {
    return { id: ++paneSeq, tabs: [], active: null, mode: 'live', kind: 'note' }
  }
  /** Ctrl+E steps live → source → reading → live in the focused pane. */
  function cycleMode() {
    const p = focused
    p.mode = MODES[(MODES.indexOf(p.mode) + 1) % MODES.length]!
  }
  function setMode(mode: ViewMode) {
    focused.mode = mode
  }
  let focused = $derived(panes[Math.min(focusedPane, panes.length - 1)] ?? panes[0]!)
  /** The focused pane's note: everything outside the panes (commands, sidebar) acts on it. */
  let activeTab = $derived(focused.active)
  /** …and that is the note behind a rendered tab: its commands are the note's. */
  let active = $derived(activeTab && tabNote(activeTab))
  let presence = $derived(presenceByPane[focused.id] ?? [])

  /** The session behind a note id, whichever vault holds it. A file's tab is not a note, and
   *  has none: the commands that act on "the note" pass it by. */
  function sessionOf(noteId: string | null | undefined): VaultSession | undefined {
    if (solo) return solo
    if (noteId && isFileTab(noteId)) return undefined
    return workspace?.sessionForNote(noteId && tabNote(noteId)) ?? undefined
  }
  /** The session behind any tab: a note's, or the vault a file's tab names. */
  function tabSession(tab: string): VaultSession | undefined {
    const f = parseFileTab(tab)
    return f ? (workspace?.get(f.vault) ?? undefined) : sessionOf(tab)
  }
  /** What the sidebar acts on: the focused tab's vault, else the one you last touched. */
  let session = $derived(solo ?? (active ? tabSession(active) : undefined) ?? workspace?.get(focusVault) ?? workspace?.sessions[0])
  let vaults: VaultNode[] = $derived(
    (workspace?.sessions ?? []).map((s) => ({ id: s.id, label: s.label, notes: s.notes })),
  )
  /** Every folder in every vault, for the palette. Folders are path prefixes (SPEC §9), so
   *  they are derived from the notes rather than stored anywhere. */
  let folders = $derived.by(() => {
    const out: { vault: string; folder: string }[] = []
    for (const v of vaults) {
      const seen = new Set<string>()
      for (const n of v.notes) {
        const parts = n.path.split('/').slice(0, -1)
        for (let i = 1; i <= parts.length; i++) {
          const folder = parts.slice(0, i).join('/')
          if (!seen.has(folder)) {
            seen.add(folder)
            out.push({ vault: v.id, folder })
          }
        }
      }
    }
    return out
  })

  /** Vault labels are noise until there is more than one vault to tell apart. */
  let manyVaults = $derived((workspace?.sessions.length ?? 0) > 1)
  function vaultLabel(vault: string | null | undefined): string {
    return manyVaults && vault ? (workspace?.label(vault) ?? '') : ''
  }
  function labelOfNote(noteId: string): string {
    return vaultLabel(workspace?.vaultOfNote(tabNote(noteId)))
  }

  // The route follows the focused note, so a reload or a copied URL comes back to it.
  $effect(() => {
    if (publicToken || noteOnly || invite) return
    const vault = session?.id
    const note = activeTab
    // A detached window with its last tab closed keeps its route, and with it what it is.
    const want = note && vault ? `#/${detached ? 'w' : 'v'}/${vault}/${note}` : vault && !detached ? `#/v/${vault}` : ''
    if (want && location.hash !== want) location.hash = want
  })

  // ---- layout persistence, per device, across every vault
  interface StoredLayout {
    panes?: { tabs?: string[]; active?: string | null; mode?: string; kind?: string; seq?: number }[]
    focused?: number
  }
  const MODES: ViewMode[] = ['live', 'source', 'reading']
  const asMode = (m: unknown): ViewMode => (MODES.includes(m as ViewMode) ? (m as ViewMode) : 'live')
  function loadLayout(): { panes: PaneState[]; focused: number } {
    try {
      const raw = localStorage.getItem('lemmate.layout')
      const data = raw ? (JSON.parse(raw) as StoredLayout) : null
      const list = (data?.panes ?? [])
        .filter((p) => Array.isArray(p.tabs) && p.tabs.length > 0)
        .slice(0, MAX_PANES)
        .map((p) => {
          // A render pane, from before rendered notes were tabs like the rest: its tabs say so now.
          const was = p.kind === 'render' ? renderTab : (t: string) => t
          const tabs = (p.tabs ?? []).filter((t) => typeof t === 'string').map(was)
          const at = p.active ? was(p.active) : null
          const kind = p.kind === 'history' ? 'history' : 'note'
          const seq = kind === 'history' && typeof p.seq === 'number' ? p.seq : 0
          return { id: ++paneSeq, tabs, active: at && tabs.includes(at) ? at : (tabs[0] ?? null), mode: asMode(p.mode), kind, seq } as PaneState
        })
      if (list.length) return { panes: list, focused: Math.min(Math.max(data?.focused ?? 0, 0), list.length - 1) }
    } catch {
      /* ignore unreadable layouts */
    }
    return { panes: [blankPane()], focused: 0 }
  }
  function loadPinned(): string[] {
    try {
      const raw = localStorage.getItem('lemmate.pins')
      const list = raw ? (JSON.parse(raw) as unknown) : null
      return Array.isArray(list) ? list.filter((x): x is string => typeof x === 'string') : []
    } catch {
      return []
    }
  }
  $effect(() => {
    if (solo || detached || !workspace) return
    const data = JSON.stringify({
      panes: panes.map((p) => ({ tabs: p.tabs.filter((t) => !isBlank(t)), active: p.active, mode: p.mode, kind: p.kind, seq: p.seq })),
      focused: focusedPane,
    })
    try {
      localStorage.setItem('lemmate.layout', data)
    } catch {
      /* storage may be unavailable */
    }
  })
  // Once every vault doc has synced, drop restored tabs whose notes no longer exist, and open
  // the note the URL asked for now that we can tell which vault it lives in.
  $effect(() => {
    const ws = workspace
    if (solo || !ws || !ws.synced || layoutRestored) return
    const known = new Set(ws.notes.map((n) => n.id))
    untrack(() => {
      layoutRestored = true
      // A file's tab stays while its vault does: the file list is fetched only when something
      // shows it, and a file gone since says so in its own tab.
      const keep = (t: string) => {
        const f = parseFileTab(t)
        return f ? !!ws.get(f.vault) : known.has(tabNote(t)) || isBlank(t)
      }
      const kept = panes.map((p) => ({ ...p, tabs: p.tabs.filter(keep) })).map((p) => ({ ...p, active: p.active && p.tabs.includes(p.active) ? p.active : (p.tabs[0] ?? null) }))
      const live = kept.filter((p) => p.tabs.length > 0)
      panes = live.length ? live : [blankPane()]
      focusedPane = Math.min(focusedPane, panes.length - 1)
      pinned = pinned.filter(keep)
    })
  })
  // A note named by the URL (a link someone sent, a reload) opens as soon as it is known.
  $effect(() => {
    const ws = workspace
    const want = routeNote
    if (!ws || !want) return
    if (!ws.notes.some((n) => n.id === tabNote(want))) return
    untrack(() => {
      routeNote = null
      open(want)
    })
  })

  // ---- in-app prompt/confirm (native dialogs are unreliable in the Tauri webview)
  interface AskOptions {
    kind: 'prompt' | 'confirm'
    title: string
    body?: string
    initial?: string
    placeholder?: string
    suggestions?: string[]
    confirmLabel?: string
    danger?: boolean
  }
  let modal = $state<(AskOptions & { settle: (value: string | null) => void }) | null>(null)
  /** Show a modal dialog; resolves with the entered value ('' for confirms) or null when cancelled. */
  function ask(opts: AskOptions): Promise<string | null> {
    return new Promise((resolve) => {
      modal = { ...opts, settle: resolve }
    })
  }
  function closeModal(value: string | null) {
    const m = modal
    modal = null
    m?.settle(value)
  }

  /**
   * Open a note in the focused pane, **reusing its active tab**: browsing the tree is how you
   * look for something, and it should not leave a trail of tabs to close afterwards. The tab
   * you displace joins the reopen stack, so Ctrl+Shift+T undoes a misclick.
   *
   * Two tabs are never displaced: a pinned one (that is what pinning is for) and one already
   * showing the note. `openInNewTab` is the deliberate opposite, from the ＋ button and the
   * right-click menu.
   */
  /** Move the focus off a history pane, or a rendered note, before opening a note — into a pane
   *  with a note in front, made for it if the last one was closed (`notePane`). */
  function noteFocus() {
    const next = notePane(panes, focusedPane, MAX_PANES, blankPane)
    if (next.panes !== panes) panes = next.panes
    focusedPane = next.focused
  }
  function open(id: string) {
    noteFocus()
    const p = focused
    if (!p.tabs.includes(id)) {
      const at = p.active ? p.tabs.indexOf(p.active) : -1
      // A render is not browsed past either: making it again is not a click away.
      if (at === -1 || pinned.includes(p.active!) || isRenderTab(p.active!)) p.tabs = [...p.tabs, id]
      else {
        const displaced = p.tabs[at]!
        p.tabs = p.tabs.map((t, i) => (i === at ? id : t))
        if (!isBlank(displaced)) closed = [...closed.filter((c) => c !== displaced), displaced].slice(-20)
      }
    }
    landOn(id)
  }
  /** Open a note in a tab of its own, leaving whatever is already open where it is. */
  function openInNewTab(id: string) {
    noteFocus()
    const p = focused
    if (!p.tabs.includes(id)) p.tabs = [...p.tabs, id]
    landOn(id)
  }
  /** A fresh empty tab, waiting for the next note you click (the ＋ on the tab strip). */
  function newTab() {
    // Only off a history pane: a ＋ beside a render is asked for there.
    if (focused.kind === 'history') noteFocus()
    const p = focused
    p.tabs = [...p.tabs, `blank:${++blankSeq}`]
    p.active = p.tabs[p.tabs.length - 1]!
  }
  function landOn(id: string) {
    const p = focused
    p.active = id
    palette = null
    // On a phone the drawer is covering the note you just picked.
    drawer = false
    closed = closed.filter((c) => c !== id)
    const vault = workspace?.vaultOfNote(tabNote(id))
    if (vault) focusVault = vault
    tagsVersion++
  }
  function openPath(vault: string, path: string) {
    const id = workspace?.get(vault)?.idOf(path)
    if (id) open(id)
  }
  /** Close a tab wherever it is open; the pane goes away with its last tab. */
  function close(id: string, force = false) {
    if (!force && pinned.includes(id)) return
    const p = panes.find((x) => x === focused && x.tabs.includes(id)) ?? panes.find((x) => x.tabs.includes(id))
    if (!p) return
    const i = p.tabs.indexOf(id)
    p.tabs = p.tabs.filter((t) => t !== id)
    if (p.active === id) p.active = p.tabs[Math.min(i, p.tabs.length - 1)] ?? null
    if (!isBlank(id)) closed = [...closed.filter((c) => c !== id), id].slice(-20)
    if (p.tabs.length === 0 && panes.length > 1) closePane(panes.indexOf(p))
  }
  function splitRight(at = focusedPane) {
    const p = panes[at]
    if (solo || narrow.current || panes.length >= MAX_PANES || !p) return
    const id = p.active
    if (!id) return
    panes = [...panes.slice(0, at + 1), { id: ++paneSeq, tabs: [id], active: id, mode: p.mode, kind: 'note' }, ...panes.slice(at + 1)]
    focusedPane = at + 1
  }
  /**
   * A note's history, in a pane beside it (SPEC §9). It splits where there is room and reuses
   * the last pane where there is not — the same rule `openInNewPane` follows — and a second
   * press goes back to the pane already showing it rather than opening another.
   */
  function openHistory(at = focusedPane) {
    const p = panes[at]
    const id = p?.active && tabNote(p.active)
    if (!p || !id || isBlank(id) || isFileTab(id) || solo) return
    const seen = panes.findIndex((q) => q.kind === 'history' && q.active === id)
    if (seen >= 0) {
      focusedPane = seen
      return
    }
    const entry: PaneState = { id: ++paneSeq, tabs: [id], active: id, mode: p.mode, kind: 'history', seq: 0 }
    if (panes.length < MAX_PANES) {
      panes = [...panes.slice(0, at + 1), entry, ...panes.slice(at + 1)]
      focusedPane = at + 1
    } else {
      panes = [...panes.slice(0, -1), entry]
      focusedPane = panes.length - 1
    }
  }
  /**
   * What Quarto makes of the note (SPEC §5.6): a tab like any other (lib/rendertabs.ts), opened
   * in a pane beside the note. Renders gather: the next one joins a pane already showing one,
   * and a note rendered already goes back to its tab rather than opening another.
   */
  function openRender(at = focusedPane) {
    const p = panes[at]
    const id = p?.active && tabNote(p.active)
    if (!p || !id || isBlank(id) || isFileTab(id) || solo) return
    const tab = renderTab(id)
    const seen = panes.findIndex((q) => q.kind !== 'history' && q.tabs.includes(tab))
    const home = seen >= 0 ? seen : panes.findIndex((q) => q.kind !== 'history' && !!q.active && isRenderTab(q.active))
    if (home >= 0) {
      const r = panes[home]!
      if (!r.tabs.includes(tab)) r.tabs = [...r.tabs, tab]
      r.active = tab
      focusedPane = home
      return
    }
    const next = inNewPane(panes, at, MAX_PANES, tab, (t, like) => ({ id: ++paneSeq, tabs: [t], active: t, mode: like.mode, kind: 'note' }))
    panes = next.panes
    focusedPane = next.focused
  }
  function closePane(i = focusedPane) {
    if (panes.length <= 1) return
    panes = panes.filter((_, j) => j !== i)
    focusedPane = Math.min(focusedPane > i ? focusedPane - 1 : focusedPane, panes.length - 1)
  }
  function focusPane(delta: number) {
    focusedPane = (focusedPane + delta + panes.length) % panes.length
  }
  function reopenClosed() {
    const id = closed[closed.length - 1]
    closed = closed.slice(0, -1)
    if (id && sessionOf(id)?.pathOf(tabNote(id))) openInNewTab(id)
  }
  function togglePin(id: string) {
    // Re-read first: another window on this origin may have pinned or unpinned something since
    // this one loaded, and writing our copy back would undo it. Our copy is only the fallback
    // for storage that holds nothing.
    let current = pinned
    try {
      if (localStorage.getItem('lemmate.pins') !== null) current = loadPinned()
    } catch {
      /* storage may be unavailable */
    }
    pinned = current.includes(id) ? current.filter((p) => p !== id) : [...current, id]
    try {
      localStorage.setItem('lemmate.pins', JSON.stringify(pinned))
    } catch {
      /* storage may be unavailable */
    }
  }
  function bookmarkActive() {
    if (active) bookmarkNote(sessionOf(active)?.id ?? '', active)
  }
  function bookmarkNote(vault: string, id: string) {
    const s = workspace?.get(vault) ?? sessionOf(id)
    const path = s?.pathOf(id)
    if (s && path) s.toggleBookmark({ kind: 'note', target: path, label: displayName(path) })
  }
  /** What the desktop shell hands every window it opens (`SHELL_SCRIPT` in crates/desktop); absent
   *  in a browser. */
  const shell = (window as unknown as { lemmateShell?: { openWindow?: (route: string, x?: number, y?: number) => void; closeWindow?: () => void } }).lemmateShell

  /**
   * Move a tab out into a window of its own (the `#/w/` route): from a menu, or dragged out of
   * every window, in which case `from` says which pane it left and where on the screen it was let
   * go. The desktop shell opens another app window on the same relay; a browser opens a popup, and
   * if it blocks that — a drag's end is not a click — the tab stays where it is. Otherwise the tab
   * leaves its pane, and not onto the reopen stack: the note is not closed, it is over there.
   */
  function detach(id: string, from?: { pane: number | null; x?: number; y?: number }) {
    const vault = sessionOf(id)?.id
    if (!vault || isBlank(id)) return
    const route = `#/w/${vault}/${id}`
    // Put the new window's tab strip under the pointer, roughly where the tab was let go.
    const x = from?.x === undefined ? undefined : from.x - 80
    const y = from?.y === undefined ? undefined : from.y - 20
    if (shell?.openWindow) shell.openWindow(route, x, y)
    else {
      const at = x === undefined || y === undefined ? '' : `,left=${x},top=${y}`
      if (!window.open(`${location.pathname}${location.search}${route}`, '_blank', `popup,width=960,height=900${at}`)) return
    }
    if (from?.pane != null) {
      panes = removeTab(panes, { tab: id, pane: from.pane })
      focusedPane = Math.min(focusedPane, panes.length - 1)
    } else close(id, true)
    closed = closed.filter((c) => c !== id)
  }
  /** Open a note beside the current one, splitting if there is room and reusing a pane if not. */
  function openInNewPane(id: string) {
    if (solo) return openInNewTab(id)
    const next = inNewPane(panes, panes.indexOf(focused), MAX_PANES, id, (tab, like) => ({ id: ++paneSeq, tabs: [tab], active: tab, mode: like.mode }))
    panes = next.panes
    focusedPane = next.focused
    landOn(id)
  }
  /** What a pane is showing, in a few words: the pane switcher's rows. */
  function paneLabel(p: PaneState): string {
    const id = p.active
    const name = !id || isBlank(id)
      ? 'New tab'
      : isFileTab(id)
        ? (parseFileTab(id)?.path.split('/').pop() ?? 'File')
        : displayName(sessionOf(id)?.pathOf(tabNote(id)) ?? '') || 'Note'
    return p.kind === 'history' ? `History · ${name}` : id && isRenderTab(id) ? `Render · ${name}` : name
  }
  /**
   * The phone's way between panes. It draws only the focused one, so the others need a door:
   * the chip on the top bar opens this list of them, anchored under it, the current one ticked.
   */
  function paneMenu(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect()
    menu = {
      x: r.left,
      y: r.bottom + 4,
      items: [
        ...panes.map((p, i) => ({ label: paneLabel(p), checked: i === focusedPane, run: () => (focusedPane = i) })),
        { separator: true, label: '' },
        { label: 'Close this pane', run: () => closePane() },
      ],
    }
  }
  /** A tab dragged within this window or in from another (lib/tabmoves.ts); a split needs room
   *  for another pane. Answers whether the tab was taken. */
  function dropTab(drag: TabDrag, drop: TabDrop): boolean {
    // From another window, only a note this one holds too: a blank tab, or a note this workspace
    // cannot open (not synced yet, another account's), stays in the window it came from.
    if (drag.pane === null && (isBlank(drag.tab) || !sessionOf(drag.tab)?.pathOf(tabNote(drag.tab)))) return false
    const room = solo || narrow.current ? panes.length : MAX_PANES
    const moved = moveTab(panes, drag, drop, pinned, room, (tab, like) => ({ id: ++paneSeq, tabs: [tab], active: tab, mode: like.mode, kind: 'note' }))
    if (!moved) return false
    panes = moved.panes
    focusedPane = moved.focused
    closed = closed.filter((c) => c !== drag.tab)
    return true
  }
  /**
   * Another window took it. It still goes on the reopen stack: the window learns of the drop only
   * from the drag's final `dropEffect`, and if an engine ever reports a drop that did not happen,
   * Ctrl+Shift+T is the way back — the worst case is the note open in both windows.
   */
  function tabGone(drag: TabDrag) {
    panes = removeTab(panes, drag)
    focusedPane = Math.min(focusedPane, panes.length - 1)
    if (!isBlank(drag.tab)) closed = [...closed.filter((c) => c !== drag.tab), drag.tab].slice(-20)
  }
  /** A phone has no second window to put a note in: `window.open` there is just another tab. */
  let canDetach = $derived(!solo && !narrow.current)
  let commands: Command[] = $derived([
    { id: 'open', label: 'Open or create note…', shortcut: 'Ctrl+O', run: () => (palette = '') },
    { id: 'daily', label: "Open today's daily note", shortcut: 'Ctrl+Shift+D', run: () => daily() },
    { id: 'daily-prev', label: 'Previous daily note', shortcut: 'Alt+[', run: () => stepDaily(-1) },
    { id: 'daily-next', label: 'Next daily note', shortcut: 'Alt+]', run: () => stepDaily(1) },
    { id: 'calendar', label: 'Daily notes calendar and settings…', run: openCalendar },
    { id: 'search', label: 'Search all vaults', shortcut: 'Ctrl+Shift+F', run: () => (palette = '') },
    { id: 'files', label: 'Show files', run: () => show('files') },
    { id: 'tags', label: 'Show tags', run: () => show('tags') },
    { id: 'bookmarks', label: 'Show bookmarks', run: () => show('bookmarks') },
    { id: 'history', label: 'Show version history', shortcut: 'Ctrl+Shift+R', run: () => openHistory() },
    { id: 'trash', label: 'Show trash', run: () => show('trash') },
    { id: 'attachments', label: 'Show attachments', run: () => show('attachments') },
    { id: 'upload', label: 'Upload files…', run: () => session && startUpload(session.id, null) },
    { id: 'newtab', label: 'New tab', shortcut: 'Ctrl+T', run: newTab },
    { id: 'mode-cycle', label: 'Cycle view mode (live / source / reading)', shortcut: 'Ctrl+E', run: cycleMode },
    { id: 'mode-live', label: 'View: live preview', run: () => setMode('live') },
    { id: 'mode-source', label: 'View: source', run: () => setMode('source') },
    { id: 'mode-reading', label: 'View: reading', run: () => setMode('reading') },
    ...(onRelay ? [] : [{ id: 'share', label: 'Share note…', run: () => (shareOpen = !!active) }]),
    ...(localOnly && canConnect ? [{ id: 'connect', label: 'Connect a server…', run: () => (connectOpen = true) }] : []),
    // Only a relay can merge vaults: it is the one thing that holds both engines (SPEC §3.2).
    ...(onRelay && vaults.length > 1 ? [{ id: 'merge', label: 'Merge a vault into another…', run: () => (mergeOpen = true) }] : []),
    { id: 'export-html', label: 'Export note as HTML', run: () => exportActive('html') },
    { id: 'export-docx', label: 'Export note as DOCX', run: () => exportActive('docx') },
    { id: 'export-pdf', label: 'Export note as PDF', run: () => exportActive('pdf') },
    { id: 'export-slides', label: 'Export note as slides (reveal.js)', run: () => exportActive('revealjs') },
    { id: 'render', label: 'Render with Quarto', run: () => openRender() },
    { id: 'render-pdf', label: 'Render with Quarto as PDF', run: () => renderActive('pdf') },
    { id: 'render-docx', label: 'Render with Quarto as Word document', run: () => renderActive('docx') },
    { id: 'render-slides', label: 'Render with Quarto as slides (reveal.js)', run: () => renderActive('revealjs') },
    { id: 'bookmark', label: session && active && session.isBookmarked('note', session.pathOf(active) ?? '') ? 'Remove bookmark' : 'Bookmark this note', shortcut: 'Ctrl+Shift+B', run: bookmarkActive },
    { id: 'rename', label: 'Rename / move note', run: renameActive },
    { id: 'delete', label: 'Move note to trash', run: deleteActive },
    { id: 'close', label: 'Close tab', shortcut: 'Ctrl+W', run: () => activeTab && close(activeTab) },
    ...(canDetach ? [{ id: 'detach', label: 'Move tab to new window', run: () => activeTab && detach(activeTab) }] : []),
    { id: 'pin', label: activeTab && pinned.includes(activeTab) ? 'Unpin tab' : 'Pin tab', run: () => activeTab && togglePin(activeTab) },
    { id: 'reopen', label: 'Reopen closed tab', shortcut: 'Ctrl+Shift+T', run: reopenClosed },
    { id: 'split', label: 'Split right', shortcut: 'Ctrl+\\', run: splitRight },
    { id: 'closepane', label: 'Close pane', run: () => closePane() },
    { id: 'nextpane', label: 'Focus next pane', shortcut: 'Ctrl+Alt+→', run: () => focusPane(1) },
    { id: 'prevpane', label: 'Focus previous pane', shortcut: 'Ctrl+Alt+←', run: () => focusPane(-1) },
    { id: 'newvault', label: 'New vault…', run: newVault },
    ...(session ? [{ id: 'renamevault', label: `Rename vault “${session.label}”…`, run: () => renameVault(session!.id) }] : []),
    { id: 'import', label: 'Import an Obsidian vault…', run: () => (importInto = session?.id ?? null) },
    ...(me && me.id !== 'local' ? [{ id: 'account', label: 'Account, password, tokens and invites…', run: () => (accountOpen = true) }] : []),
    ...(me && me.id !== 'local' ? [{ id: 'signout', label: `Sign out (${me.email})`, run: signOut }] : []),
  ])
  /**
   * Templates (SPEC §9): a note — `Templates/Note.md`, the vault's daily template — with
   * {{date}}, {{date:FORMAT}}, {{time}}, {{title}}. `day` is the date {{date}} means: a daily
   * note's own day, else today.
   */
  async function template(vault: VaultSession, path: string, title: string, fallback: string, day?: Day): Promise<string> {
    const id = vault.idOf(path)
    if (!id) return fallback
    const { doc, release } = vault.acquire(id)
    try {
      await new Promise<void>((resolve) => {
        if (doc.getText('content').length > 0 || vault.client.isSynced(id)) return resolve()
        const t = setTimeout(resolve, 3000)
        doc.getText('content').observe(() => (clearTimeout(t), resolve()))
      })
      const raw = doc.getText('content').toString()
      const body = raw.startsWith('---\n') ? raw.slice(raw.indexOf('\n---', 4) + 4).replace(/^\n/u, '') : raw
      return fillTemplate(body, title, day)
    } finally {
      release()
    }
  }
  function fillTemplate(body: string, title: string, day?: Day): string {
    const d = new Date()
    const pad = (n: number) => String(n).padStart(2, '0')
    // The date tokens are Moment's (lib/daily.ts), which leaves `HH` and `mm` alone for these.
    const fmt = (f: string) =>
      formatDate(f, day ?? today(d))
        .replace(/HH/gu, pad(d.getHours()))
        .replace(/mm/gu, pad(d.getMinutes()))
    return body
      .replace(/\{\{date:([^}]+)\}\}/gu, (_, f: string) => fmt(f))
      .replace(/\{\{date\}\}/gu, fmt('YYYY-MM-DD'))
      .replace(/\{\{time\}\}/gu, fmt('HH:mm'))
      .replace(/\{\{title\}\}/gu, title)
      .replace(/\{\{cursor\}\}/gu, '')
  }
  async function create(typed: string, vault: string | undefined = session?.id) {
    const s = workspace?.get(vault) ?? (solo ? undefined : undefined)
    if (!s) return
    const path = notePath(typed)
    const title = displayName(path)
    open(s.createNote(path, await template(s, 'Templates/Note.md', title, `# ${title}\n\n`)))
  }
  /** The daily note for a day (today by default), created from the vault's template if missing. */
  async function daily(day: Day = today()) {
    const s = session
    if (!s) return
    const path = pathFor(s.daily, day)
    const existing = s.idOf(path)
    const name = iso(day)
    open(existing ?? s.createNote(path, await template(s, templateOf(s.daily), name, `# ${name}\n\n`, day)))
  }
  /**
   * Step to the previous or next daily note that exists, counting from the one open (or from
   * today when the open note is not a daily note). Nothing is created: this is for reading back.
   */
  function stepDaily(dir: -1 | 1) {
    const s = session
    if (!s) return
    const from = (active && dayOf(s.daily, s.pathOf(active) ?? '')) || today()
    let best: { day: Day; id: string } | null = null
    for (const n of s.notes) {
      const d = dayOf(s.daily, n.path)
      if (!d || compareDays(d, from) * dir <= 0) continue
      if (!best || compareDays(d, best.day) * dir < 0) best = { day: d, id: n.id }
    }
    if (best) open(best.id)
  }
  function openCalendar() {
    const s = session
    calendarAt = (s && active && dayOf(s.daily, s.pathOf(active) ?? '')) || today()
  }
  function renameActive() {
    if (active) void renameNote(sessionOf(active)?.id ?? '', active)
  }
  async function renameNote(vault: string, id: string) {
    const s = workspace?.get(vault) ?? sessionOf(id)
    if (!s) return
    const current = s.pathOf(id) ?? ''
    const next = (await ask({ kind: 'prompt', title: 'Rename / move note', initial: current, placeholder: 'folder/note.md' }))?.trim()
    if (next && next !== current) s.renameNote(id, next.endsWith('.md') || next.endsWith('.qmd') ? next : `${next}.md`)
  }
  function deleteActive() {
    if (active) void trashNotes(sessionOf(active)?.id ?? '', [active])
  }
  /** Move notes to trash — one from the note header, or a whole selection from the tree. */
  async function trashNotes(vault: string, ids: string[]) {
    const s = workspace?.get(vault) ?? sessionOf(ids[0])
    if (!s || ids.length === 0) return
    const title =
      ids.length === 1
        ? `Move “${s.pathOf(ids[0]!) ?? ids[0]}” to trash?`
        : `Move ${ids.length} notes to trash?`
    const ok = await ask({ kind: 'confirm', title, confirmLabel: 'Move to trash', danger: true })
    if (ok === null) return
    for (const id of ids) {
      s.deleteNote(id)
      // The note is gone: close it, and its render, in every pane, pinned or not.
      for (const tab of [id, renderTab(id)]) {
        for (const p of [...panes]) if (p.tabs.includes(tab)) close(tab, true)
        closed = closed.filter((c) => c !== tab)
      }
    }
  }

  /**
   * A drop in the file browser. Inside one vault it is a rename per note and links follow it
   * (SPEC §4.4); into another vault it is a copy plus a delete, which changes note ids and
   * leaves `[[links]]` from the source vault dangling — so that one is confirmed first.
   */
  async function moveDropped(drag: DragPayload, toVault: string, folder: string) {
    const ws = workspace
    if (!ws) return
    const moves = plan(drag, folder, (id) => ws.pathOf(id))
    if (moves.length === 0) return
    const what = drag.folder !== undefined ? `“${drag.folder}” and its ${moves.length} notes` : `${moves.length} note${moves.length === 1 ? '' : 's'}`
    const where = folder ? `“${folder}”` : `the root of ${ws.label(toVault)}`
    if (drag.vault !== toVault) {
      const ok = await ask({
        kind: 'confirm',
        title: `Move ${what} to ${where} in another vault?`,
        body: 'Notes moved between vaults are copied and then deleted, so they get new ids and any [[links]] to them from the vault they leave will break. Attachments they reference are copied too.',
        confirmLabel: 'Move',
      })
      if (ok === null) return
    }
    const openBefore = moves.filter((m) => panes.some((p) => p.tabs.includes(m.id))).map((m) => m.id)
    const { moved, failed } = await ws.moveNotes(moves, drag.vault, toVault)
    if (drag.vault !== toVault) {
      // The originals are gone and their ids with them; reopen the copies in their place.
      for (const id of openBefore) close(id, true)
      // Their renders name the old ids too, and a render is one click to make again.
      for (const m of moves) for (const p of [...panes]) if (p.tabs.includes(renderTab(m.id))) close(renderTab(m.id), true)
      const reopen = moved.filter((_, i) => openBefore.includes(moves[i]?.id ?? ''))
      for (const m of reopen) openInNewTab(m.id)
    }
    if (failed.length) {
      await ask({
        kind: 'confirm',
        title: `${failed.length} of ${moves.length} could not be moved`,
        body: failed.map((f) => `${f.path}: ${f.error}`).join('\n'),
        confirmLabel: 'OK',
      })
    }
    tagsVersion++
  }

  // ---- folder actions (folders are just path prefixes; SPEC §9)
  async function createInVault(vault: string) {
    focusVault = vault
    const name = await ask({ kind: 'prompt', title: `New note in ${workspace?.label(vault) ?? 'vault'}`, placeholder: 'Title' })
    if (name?.trim()) create(name.trim(), vault)
  }
  async function createInFolder(vault: string, folder: string) {
    const name = await ask({ kind: 'prompt', title: `New note in ${folder}/`, placeholder: 'Title' })
    if (name?.trim()) create(`${folder}/${name.trim()}`, vault)
  }
  async function renameFolder(vault: string, folder: string) {
    const s = workspace?.get(vault)
    if (!s) return
    const next = (await ask({ kind: 'prompt', title: 'Rename / move folder', initial: folder }))?.trim().replace(/^\/+|\/+$/gu, '')
    if (!next || next === folder) return
    for (const n of s.notes.filter((n) => n.path.startsWith(`${folder}/`))) {
      await s.renameNote(n.id, `${next}/${n.path.slice(folder.length + 1)}`)
    }
  }
  async function deleteFolder(vault: string, folder: string) {
    const s = workspace?.get(vault)
    if (!s) return
    const inside = s.notes.filter((n) => n.path.startsWith(`${folder}/`))
    const ok = await ask({ kind: 'confirm', title: `Move “${folder}” and its ${inside.length} notes to trash?`, confirmLabel: 'Move to trash', danger: true })
    if (ok === null) return
    for (const n of inside) {
      close(n.id, true)
      s.deleteNote(n.id)
    }
  }

  // ---- files that are not notes (SPEC §9)

  function openFile(vault: string, path: string) {
    open(fileTab(vault, path))
  }

  let uploadTarget = $derived(uploading ? workspace?.get(uploading.vault) : undefined)
  /** The note the upload dialog offers "next to": the focused one, when it is in that vault. */
  let uploadNote = $derived(
    uploadTarget && active && !isFileTab(active) && sessionOf(active)?.id === uploadTarget.id
      ? { id: active, path: uploadTarget.pathOf(active) ?? '' }
      : null,
  )

  /** Pick files, then ask where they go. `folder`: a folder's "upload here", else `null`. */
  function startUpload(vault: string, folder: string | null) {
    const input = document.createElement('input')
    input.type = 'file'
    input.multiple = true
    input.onchange = () => {
      const files = [...(input.files ?? [])]
      if (files.length) uploading = { vault, folder, files }
    }
    input.click()
  }

  /** Every open tab of `from` now shows `to`: a file renamed under its own tab stays open. */
  function retargetTabs(from: string, to: string | null) {
    for (const p of panes) {
      if (!p.tabs.includes(from)) continue
      p.tabs = to === null ? p.tabs.filter((t) => t !== from) : p.tabs.map((t) => (t === from ? to : t))
      if (p.active === from) p.active = to ?? p.tabs[0] ?? null
    }
  }

  async function renameFile(vault: string, entry: FileEntry) {
    const to = (await ask({ kind: 'prompt', title: 'Rename / move file', initial: entry.path, confirmLabel: 'Move' }))?.trim()
    if (!to || to === entry.path) return
    try {
      const moved = await api.moveFile(vault, entry.path, to)
      retargetTabs(fileTab(vault, entry.path), fileTab(vault, moved.path))
      workspace?.get(vault)?.filesChanged(0)
    } catch (e) {
      await ask({ kind: 'confirm', title: `Not moved: ${e instanceof Error ? e.message : String(e)}`, confirmLabel: 'OK' })
    }
  }

  async function deleteFile(vault: string, entry: FileEntry) {
    const s = workspace?.get(vault)
    const users = entry.used_by.map((id) => displayName(s?.pathOf(id) ?? '')).filter(Boolean)
    const body = users.length
      ? `${users.join(', ')} ${users.length === 1 ? 'uses' : 'use'} it, and will show a broken link.`
      : entry.vault
        ? 'It is part of the vault\'s own settings.'
        : ''
    const ok = await ask({ kind: 'confirm', title: `Delete ${entry.path}?`, body, confirmLabel: 'Delete', danger: true })
    if (ok === null) return
    try {
      await api.deleteFile(vault, entry.path)
      retargetTabs(fileTab(vault, entry.path), null)
      s?.filesChanged(0)
    } catch (e) {
      await ask({ kind: 'confirm', title: `Not deleted: ${e instanceof Error ? e.message : String(e)}`, confirmLabel: 'OK' })
    }
  }

  /** Server-side pandoc export (SPEC §12); the browser saves the result as a download. */
  async function exportActive(format: string) {
    const s = sessionOf(active)
    if (!s || !active) return
    const id = active
    const r = await fetch(`/api/v1/vaults/${s.id}/notes/${id}/export`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ format }),
    })
    if (r.status === 501) return void ask({ kind: 'confirm', title: 'Export needs pandoc on the server (see the deployment guide).', confirmLabel: 'OK' })
    if (!r.ok) return void ask({ kind: 'confirm', title: `Export failed (${r.status}).`, confirmLabel: 'OK' })
    const ext = r.headers.get('content-disposition')?.match(/filename="[^"]*\.([^".]+)"/u)?.[1] ?? format
    await saveResponse(r, `${displayName(s.pathOf(id) ?? 'note')}.${ext}`)
  }

  /** A Quarto render to a file (SPEC §5.6) — the page itself has a pane, `openRender`. */
  async function renderActive(format: RenderFormat) {
    const s = sessionOf(active)
    if (!s || !active) return
    const id = active
    const r = await api.render(s.id, id, format)
    if (r.status === 501) return void ask({ kind: 'confirm', title: 'Rendering needs Quarto, and there is none here — or the server has it switched off.', confirmLabel: 'OK' })
    if (!r.ok) return void ask({ kind: 'confirm', title: `Rendering failed: ${(await r.text()).trim() || r.status}`, confirmLabel: 'OK' })
    const ext = format === 'revealjs' ? 'html' : format
    await saveResponse(r, `${displayName(s.pathOf(id) ?? 'note')}.${ext}`)
  }

  /** Hand a response to the browser as a download. Named from the note here rather than from
   *  the header, which cannot carry a name outside ASCII intact. */
  async function saveResponse(r: Response, name: string) {
    const url = URL.createObjectURL(await r.blob())
    const a = document.createElement('a')
    a.href = url
    a.download = name.replace(/[/\\]/gu, '-')
    a.click()
    setTimeout(() => URL.revokeObjectURL(url), 10_000)
  }

  /** After an import, pick up the vault it went into — it may be brand new. */
  async function imported(vault: string) {
    const ws = workspace
    if (!ws) return
    ws.add(vault)
    focusVault = vault
    await ws.refresh()
    tagsVersion++
  }

  function onKey(e: KeyboardEvent) {
    if (modal) return
    // Not preventDefault'd: Escape still reaches whatever else is listening for it.
    if (e.key === 'Escape' && drawer) drawer = false
    const mod = e.ctrlKey || e.metaKey
    if (e.altKey && !mod && !e.shiftKey && (e.code === 'BracketLeft' || e.code === 'BracketRight')) {
      stepDaily(e.code === 'BracketLeft' ? -1 : 1)
      e.preventDefault()
      return
    }
    if (!mod) return
    if (e.altKey && (e.key === 'ArrowRight' || e.key === 'ArrowLeft')) {
      focusPane(e.key === 'ArrowRight' ? 1 : -1)
      e.preventDefault()
    } else if (e.key === '\\') {
      splitRight()
      e.preventDefault()
    } else if ((e.key === 'o' || e.key === 'p' || e.key === 'k') && !e.shiftKey) {
      palette = palette === null ? '' : null
      e.preventDefault()
    } else if (e.key === 'n' && !e.shiftKey) {
      palette = ''
      e.preventDefault()
    } else if (e.key === 'e' && !e.shiftKey) {
      cycleMode()
      e.preventDefault()
    } else if (e.key === 'w') {
      if (activeTab) close(activeTab)
      e.preventDefault()
    } else if ((e.key === 't' || e.key === 'T') && e.shiftKey) {
      reopenClosed()
      e.preventDefault()
    } else if (e.key === 't') {
      newTab()
      e.preventDefault()
    } else if (e.key === 'd' && e.shiftKey) {
      void daily()
      e.preventDefault()
    } else if ((e.key === 'f' || e.key === 'F') && e.shiftKey) {
      palette = ''
      e.preventDefault()
    } else if ((e.key === 'p' || e.key === 'P') && e.shiftKey) {
      palette = palette === '>' ? null : '>'
      e.preventDefault()
    } else if ((e.key === 'b' || e.key === 'B') && e.shiftKey) {
      bookmarkActive()
      e.preventDefault()
    } else if ((e.key === 'r' || e.key === 'R') && e.shiftKey) {
      openHistory()
      e.preventDefault()
    }
  }

  let activePath = $derived(session && active ? (session.pathOf(active) ?? unnamedNote(session)) : '')
  /** What the narrow top bar names, where there is no tab strip wide enough to read. */
  let activeTitle = $derived(active && !isBlank(active) && activePath ? displayName(activePath) : 'Lemmate')
  // The desktop shell names a detached window after its document, so the task switcher can
  // tell several of them apart.
  $effect(() => {
    if (detached) document.title = activeTitle
  })
  /**
   * A detached window goes away with its last tab, however the tab went — closed, moved on to
   * another window, trashed. Watched here rather than in `close` so that a tab closed and a
   * replacement opened in the same breath (a move between vaults) does not take the window with
   * it, and only once it has held a tab: it starts empty, waiting for its note to sync.
   */
  let heldTab = false
  $effect(() => {
    if (!detached) return
    const empty = panes.every((p) => p.tabs.length === 0)
    if (!empty) heldTab = true
    else if (heldTab) closeWindow()
  })
  function closeWindow() {
    // The desktop shell opened this window, so only it can close it (see `relay_window`); a
    // browser lets a page close a popup it opened itself, and ignores this otherwise.
    if (shell?.closeWindow) shell.closeWindow()
    else window.close()
  }
  let denied = $derived(solo ? solo.denied : (workspace?.denied ?? null))
  let status = $derived(solo ? solo.status : (workspace?.status ?? 'connecting'))
  let noteCount = $derived(solo ? solo.notes.length : (workspace?.noteCount ?? 0))
  let syncing = $derived(solo ? !solo.vaultSynced : !(workspace?.synced ?? false))
  // Built here rather than in the markup: an `{#if}` in the middle of a sentence eats the
  // whitespace in front of it, and this line is all conditional pieces.
  let statusLine = $derived(
    [
      // "online" is about a server, and a standalone app has none: what the socket under this
      // page reaches is the relay on this machine, so say so rather than claim connectivity.
      (localOnly && status === 'online' ? 'local' : status) + (status === 'online' && syncing ? ' · syncing…' : ''),
      `${noteCount} ${noteCount === 1 ? 'note' : 'notes'}` +
        (!solo && manyVaults ? ` in ${vaults.length} vaults` : ''),
      presence.length ? `${presence.length} editing` : '',
    ]
      .filter(Boolean)
      .join(' · '),
  )
</script>

<svelte:window onkeydown={onKey} />

{#if publicToken}
  <SharedView token={publicToken} />
{:else if setup && !setupStarting}
  <Setup status={setup} onDone={() => (setupStarting = true)} />
{:else if setupStarting}
  <main class="welcome"><h1>Lemmate</h1><p class="muted">Starting your vault…</p></main>
{:else if authRequired || (invite && !me)}
  <Login {invite} stay={signedOut} onDone={signedIn} />
{:else if appAuth}
  {#if me}<AuthorizeApp request={appAuth} {me} />{:else}<main class="welcome"><h1>Lemmate</h1><p class="muted">Loading…</p></main>{/if}
{:else if !workspace && !solo}
  <main class="welcome"><h1>Lemmate</h1><p class="muted">Loading…</p></main>
{:else}
  <div class="layout" class:narrow={narrow.current} class:detached style:--side="{sideWidth}px">
    {#if detached && denied}
      <div class="denied">
        Permission denied by the server ({denied.reason}) — your last change was not saved.
        <button class="link" onclick={() => location.reload()}>Reload</button>
        <button class="link" onclick={() => { if (workspace) workspace.denied = null }}>Dismiss</button>
      </div>
    {/if}
    {#if !detached}
      <!-- Only drawn when the sidebar is a drawer: it carries the handle that opens it, and the
           two shortcuts (new note, commands) that have no keyboard to be reached from. -->
      <header class="topbar">
        <button class="icon" onclick={() => (drawer = !drawer)} aria-expanded={drawer} aria-label="Show the sidebar">☰</button>
        <span class="here" title={activePath}>{activeTitle}</span>
        {#if panes.length > 1}
          <button class="panes" onclick={paneMenu} aria-haspopup="menu" aria-label="Pane {focusedPane + 1} of {panes.length} — switch pane" title="Switch pane">
            <Icon name="splitright" size={14} />
            {focusedPane + 1}/{panes.length}
          </button>
        {/if}
        <span class="dot" class:offline={status !== 'online'} title={statusLine}></span>
        {#if !solo}
          <button class="icon" onclick={() => daily()} oncontextmenu={(e) => (e.preventDefault(), openCalendar())} aria-label="Today's daily note"><Icon name="calendar" size={17} /></button>
        {/if}
        <button class="icon" onclick={() => (palette = '')} aria-label="Search and commands"><Icon name="search" size={17} /></button>
        <button class="icon" onclick={() => (palette = '>')} aria-label="Commands">⌘</button>
      </header>
      {#if narrow.current && drawer}
        <div class="scrim" onclick={() => (drawer = false)} role="presentation"></div>
      {/if}
      <!-- Off-screen the drawer is not just invisible but `inert`: no tab stops, nothing for a
           screen reader to wander into. -->
      <aside class:open={drawer} inert={narrow.current && !drawer}>
        <!-- The rail: every view the sidebar has, one click each, and the two things you reach
             for from anywhere (search, a new note). It replaced a search box that was a button in
             disguise and a row of tabs: the box took a row to say "press Ctrl K", and Attachments
             and Trash were hidden in the Files toolbar. On a phone it is the same rail, inside
             the drawer. A shared note on its own has no workspace for any of it to act on. -->
        {#if !solo}
          <nav class="rail" aria-label="Sidebar">
            <button onclick={() => (palette = '')} title="Search and commands (Ctrl K)" aria-label="Search and commands">
              <Icon name="search" size={18} />
            </button>
            <hr />
            <div class="views" role="tablist" aria-orientation="vertical">
              {#each RAIL as r (r.id)}
                <button class:on={view === r.id} role="tab" aria-selected={view === r.id} onclick={() => show(r.id)} title={r.label} aria-label={r.label}>
                  <Icon name={r.icon} size={18} />
                </button>
              {/each}
            </div>
            <hr />
            <!-- A phone has the daily note in its top bar, a tap away without the drawer. -->
            <button class="daily" onclick={() => daily()} oncontextmenu={(e) => (e.preventDefault(), openCalendar())} title="Today's daily note — opened, or created from the daily template (Ctrl+Shift+D). Right-click for the calendar and settings." aria-label="Today's daily note">
              <Icon name="calendar" size={18} />
            </button>
            <button onclick={() => session && createInVault(session.id)} disabled={!session} title="New note" aria-label="New note">
              <Icon name="newnote" size={18} />
            </button>
            <span class="grow"></span>
            <span class="dot" class:offline={status !== 'online'} role="status" title={statusLine} aria-label={statusLine}></span>
            <!-- Who you are, and the way back out. Both were commands and nothing else, and a
                 session you can only end by knowing what to type is a session you cannot end.
                 Absent with the relay and with `--no-auth`, where `me` is the local user and
                 there is nothing to leave. -->
            {#if me && me.id !== 'local'}
              <button
                class="avatar"
                title="{me.display_name} — {me.email}"
                aria-label="Account: {me.display_name}"
                aria-haspopup="menu"
                onclick={(e) => {
                  // Anchored to the button, not to the pointer: this one is reached from the
                  // keyboard too, and a menu that lands in the corner of the window because the
                  // click carried no coordinates is not a menu about this row.
                  const r = e.currentTarget.getBoundingClientRect()
                  menu = {
                    x: r.right + 4,
                    y: r.bottom,
                    items: [
                      { label: 'Account, password, tokens and invites…', run: () => (accountOpen = true) },
                      { separator: true, label: '' },
                      { label: 'Sign out', run: signOut, danger: true },
                    ],
                  }
                }}
              >
                {initial(me.display_name)}
              </button>
            {/if}
          </nav>
        {/if}
        <div class="panel">
          {#if solo}
            <p class="muted pad">A note shared with you. <button class="link" onclick={() => (location.hash = '')}>All vaults</button></p>
          {:else if sidebar === 'files'}
            <FilesPane
              bind:revealFolder
              {vaults}
              activeId={active}
              activeVault={session?.id ?? null}
              onOpen={open}
              bind:trash={trashOpen}
              bind:attachments={attachmentsOpen}
              actions={{
                onCreateIn: createInFolder,
                onRenameFolder: renameFolder,
                onDeleteFolder: deleteFolder,
                onCreateInVault: createInVault,
                onRenameVault: renameVault,
                onImportInto: (v) => (importInto = v),
                onNewVault: newVault,
                onRenameNote: renameNote,
                onTrashNotes: trashNotes,
                onOpenInTab: openInNewTab,
                onOpenInPane: openInNewPane,
                onShareNote: onRelay ? undefined : (id: string) => (open(id), (shareOpen = true)),
                onBookmarkNote: bookmarkNote,
                onMove: moveDropped,
              }}
            >
              {#snippet attachmentsTools()}
                <button onclick={() => attachmentsPane?.collapseAll()} title="Collapse all" aria-label="Collapse all"><Icon name="collapse" /></button>
                <button onclick={() => attachmentsPane?.locate()} disabled={!attachmentsPane?.canLocate()} title="Show the files the open note uses" aria-label="Show the files the open note uses"><Icon name="locate" /></button>
                <button onclick={() => session && startUpload(session.id, null)} disabled={!session} title="Upload files…" aria-label="Upload files"><Icon name="upload" /></button>
              {/snippet}
              {#snippet attachmentsView()}
                <AttachmentsPane
                  bind:this={attachmentsPane}
                  vaults={(workspace?.sessions ?? []).map((v) => ({ id: v.id, label: workspace?.label(v.id) ?? v.id, session: v }))}
                  activeVault={session?.id ?? null}
                  activeNoteId={active && !isFileTab(active) && !isBlank(active) ? active : null}
                  activeFile={active ? parseFileTab(active) : null}
                  onOpenFile={openFile}
                  onUpload={startUpload}
                  onRename={renameFile}
                  onDelete={deleteFile}
                />
              {/snippet}
              {#snippet trashView()}
                {#if session}
                  <TrashPane
                    vault={session.id}
                    vaults={manyVaults ? (workspace?.sessions ?? []).map((v) => ({ id: v.id, label: workspace?.label(v.id) ?? v.id })) : []}
                    version={tagsVersion}
                    onRestored={(id) => open(id)}
                  />
                {/if}
              {/snippet}
            </FilesPane>
            {#if sharedWithMe.length}
              <nav class="shared">
                <p class="muted">Shared with me</p>
                {#each sharedWithMe as n (n.id)}
                  <button onclick={() => (location.hash = `#/n/${n.vault_id}/${n.id}`)} title={n.path}>{n.title ?? displayName(n.path)}</button>
                {/each}
              </nav>
            {/if}
          {:else if sidebar === 'search'}
            <SearchPane label={labelOfNote} onOpen={open} vaults={vaults.map((v) => v.id)} />
          {:else if sidebar === 'tags'}
            <header class="panel-head"><h2>Tags</h2></header>
            {#if session}
              <TagsPane
                vault={session.id}
                version={tagsVersion}
                bind:selected={tagFilter}
                onOpen={open}
                onMenu={(t, e) => tagMenu(t, session.id, e)}
              />
            {/if}
          {:else}
            <header class="panel-head"><h2>Starred</h2></header>
            <nav class="bookmarks-pane">
              {#each workspace?.bookmarks ?? [] as b, i (b.vault + b.kind + b.target + i)}
                <button onclick={() => openPath(b.vault, b.target)} title={`${workspace?.label(b.vault)} · ${b.target}`}>
                  ★ {b.label}{#if manyVaults}<span class="vault-tag">{workspace?.label(b.vault)}</span>{/if}
                </button>
              {/each}
              {#if (workspace?.bookmarks.length ?? 0) === 0}<p class="muted pad">Bookmark a note with <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>B</kbd>.</p>{/if}
            </nav>
          {/if}
          {#if denied}
            <div class="denied">
              Permission denied by the server ({denied.reason}) — your last change was not saved.
              <button class="link" onclick={() => location.reload()}>Reload</button>
              <button class="link" onclick={() => { if (solo) solo.denied = null; else if (workspace) workspace.denied = null }}>Dismiss</button>
            </div>
          {/if}
          {#if solo}
            <footer class="status" class:offline={status !== 'online'}>
              <span class="dot"></span>
              {statusLine}
            </footer>
          {/if}
        </div>
      </aside>
      <!-- A window splitter is a focusable `separator` per ARIA; svelte's rule only knows the
           static kind. -->
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
      <div
        class="vsplit"
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize the sidebar"
        aria-valuenow={sideWidth}
        aria-valuemin={SIDE_MIN}
        aria-valuemax={SIDE_MAX}
        tabindex="0"
        onpointerdown={(e) =>
          dragResize(e, { axis: 'x', from: sideWidth, min: SIDE_MIN, max: SIDE_MAX, onMove: (v) => (sideWidth = v), onEnd: saveSideWidth })}
        onkeydown={(e) => {
          const step = e.key === 'ArrowLeft' ? -16 : e.key === 'ArrowRight' ? 16 : 0
          if (!step) return
          e.preventDefault()
          saveSideWidth(clamp(sideWidth + step, SIDE_MIN, SIDE_MAX))
        }}
        ondblclick={() => saveSideWidth(SIDE_DEFAULT)}
      ></div>
    {/if}
    <section class="main">
      {#each panes as p, i (p.id)}
        <Pane
          lookup={tabSession}
          vaultLabel={labelOfNote}
          pane={p}
          focused={i === focusedPane}
          {pinned}
          onActivate={(id) => {
            focusedPane = i
            // A history pane's tab only brings its own view forward.
            if (p.kind === 'history') p.active = id
            else landOn(id)
          }}
          onClose={(id) => { focusedPane = i; close(id) }}
          onFocus={() => (focusedPane = i)}
          onBookmark={bookmarkActive}
          onShare={onRelay ? undefined : () => (shareOpen = true)}
          onRename={renameActive}
          onDelete={deleteActive}
          onTag={(t) => { focusedPane = i; filterByTag(t) }}
          onTagMenu={(t, id, e) => { focusedPane = i; tagMenu(t, sessionOf(id)?.id ?? '', e, id) }}
          onOpen={(id) => { focusedPane = i; open(id) }}
          onPresence={(names) => (presenceByPane[p.id] = names)}
          onMode={(m) => { focusedPane = i; p.mode = m }}
          onNewTab={() => { focusedPane = i; newTab() }}
          onSplit={solo || narrow.current ? undefined : () => splitRight(i)}
          splitFull={panes.length >= MAX_PANES}
          onClosePane={panes.length > 1 ? () => closePane(i) : undefined}
          onDetach={canDetach ? detach : undefined}
          onTabDrop={dropTab}
          onTabGone={tabGone}
          onTabOut={canDetach ? (drag, x, y) => detach(drag.tab, { pane: drag.pane, x, y }) : undefined}
          onPin={togglePin}
          onHistory={solo ? undefined : () => openHistory(i)}
          historyOpen={!!p.active && panes.some((q) => q.kind === 'history' && q.active === tabNote(p.active!))}
          onRender={solo ? undefined : () => openRender(i)}
          renderOpen={!!p.active && panes.some((q) => q.kind !== 'history' && q.tabs.includes(renderTab(tabNote(p.active!))))}
          onRenameFile={renameFile}
          onDeleteFile={deleteFile}
          onOpenFile={openFile}
          onUploadFiles={(vault, folder) => startUpload(vault, folder)}
          onSeq={(seq) => { focusedPane = i; p.seq = seq }}
          onAsk={(title, initial, opts) => ask({ kind: 'prompt', title, initial, ...opts })}
        />
      {/each}
    </section>
  </div>
  {#if palette !== null && workspace}
    <!-- Keyed on the seed so Ctrl+Shift+P over an already-open palette re-opens it on
         commands rather than leaving the box as the user last typed it. -->
    {#key palette}
      <Palette
        notes={workspace.notes}
        {folders}
        {commands}
        label={vaultLabel}
        createVault={session?.id ?? null}
        initial={palette}
        onOpen={open}
        onOpenInPane={openInNewPane}
        onCreate={(path) => create(path)}
        onFolder={(vault, folder) => { focusVault = vault; sidebar = 'files'; drawer = false; revealFolder?.(vault, folder) }}
        onClose={() => (palette = null)}
      />
    {/key}
  {/if}
  {#if connectOpen}
    <ConnectServer {configPath} onClose={() => (connectOpen = false)} />
  {/if}
  {#if mergeOpen}
    <MergeVaults
      vaults={vaults.map((v) => ({ id: v.id, label: v.label, notes: v.notes.length }))}
      initialFrom={session?.id ?? null}
      onClose={() => (mergeOpen = false)}
    />
  {/if}
  {#if shareOpen && active && session}
    <ShareDialog vault={session.id} noteId={active} path={activePath} onClose={() => (shareOpen = false)} />
  {/if}
  {#if uploading && uploadTarget}
    <UploadDialog
      session={uploadTarget}
      files={uploading.files}
      folder={uploading.folder}
      note={uploadNote}
      vaultLabel={manyVaults ? workspace?.label(uploadTarget.id) : undefined}
      onClose={() => (uploading = null)}
      onDone={(paths) => {
        const vault = uploadTarget!.id
        uploading = null
        if (paths.length === 1) openFile(vault, paths[0]!)
      }}
    />
  {/if}
  {#if calendarAt && session}
    {@const s = session}
    <DailyDialog
      settings={s.daily}
      exists={(path) => !!s.idOf(path)}
      initial={calendarAt}
      onOpen={(day) => { calendarAt = null; void daily(day) }}
      onSave={(next) => s.setDaily(next)}
      onClose={() => (calendarAt = null)}
    />
  {/if}
  {#if importInto !== undefined && workspace}
    <ImportDialog
      vaults={vaults.map((v) => ({ id: v.id, label: v.label }))}
      target={importInto}
      onClose={() => (importInto = undefined)}
      onImported={imported}
    />
  {/if}
{/if}

{#if accountOpen && me}
  <AccountDialog {me} vaults={vaults.map((v) => ({ id: v.id, label: v.label }))} onClose={() => (accountOpen = false)} />
{/if}

{#if menu}
  <ContextMenu {menu} onClose={() => (menu = null)} />
{/if}

{#if modal}
  <Modal
    title={modal.title}
    body={modal.body}
    kind={modal.kind}
    initial={modal.initial}
    placeholder={modal.placeholder}
    suggestions={modal.suggestions}
    confirmLabel={modal.confirmLabel}
    danger={modal.danger}
    onSubmit={(value) => closeModal(value)}
    onCancel={() => closeModal(null)}
  />
{/if}

<style>
  .welcome {
    max-width: 30rem;
    margin: 10vh auto;
    padding: 1rem;
  }
  .muted {
    color: var(--muted);
  }
  .layout {
    display: grid;
    /* The divider draws the border between the two, so it can light up while you drag it.
       `min(…, 60vw)` caps a width dragged wide on a big monitor and then reopened in a small
       window: the stored preference survives, it just cannot eat the editor. */
    grid-template-columns: min(var(--side, 17rem), 60vw) auto 1fr;
    grid-template-areas: 'side split main';
    height: 100%;
  }
  aside {
    grid-area: side;
    background: var(--panel);
    display: flex;
    min-height: 0;
    min-width: 0;
  }
  /* Grab area wider than the hairline it draws, so the drag is not a pixel hunt. */
  .vsplit {
    grid-area: split;
    width: 7px;
    margin: 0 -3px;
    cursor: col-resize;
    position: relative;
    z-index: 1;
    /* Own the gesture: without this a drag on a touchscreen scrolls the sidebar instead. */
    touch-action: none;
  }
  .vsplit::after {
    content: '';
    position: absolute;
    inset: 0 3px;
    background: var(--border);
  }
  .vsplit:hover::after,
  .vsplit:focus-visible::after {
    background: var(--accent);
  }
  /* The rail and the view beside it. The rail is the width of its buttons and never grows;
     the panel takes the rest of the sidebar, whatever width it was dragged to. In the panel
     the view fills and whatever follows it (shared notes, a refusal, the status) sits under. */
  .panel {
    flex: 1;
    min-width: 0;
    display: grid;
    grid-template-rows: minmax(0, 1fr);
    grid-auto-rows: auto;
  }
  .panel:has(> .panel-head) {
    grid-template-rows: auto minmax(0, 1fr);
  }
  .rail {
    flex: none;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.25rem;
    width: 2.875rem;
    padding: 0.625rem 0;
    background: var(--chrome);
    border-right: 1px solid var(--border);
  }
  .rail .views {
    display: contents;
  }
  .rail hr {
    width: 1.5rem;
    margin: 0.25rem 0;
    border: 0;
    border-top: 1px solid var(--border);
  }
  .rail button {
    position: relative;
    display: grid;
    place-items: center;
    width: 2.25rem;
    height: 2.25rem;
    padding: 0;
    border: 0;
    border-radius: 8px;
    background: none;
    color: var(--muted);
    cursor: pointer;
  }
  .rail button:hover:not(:disabled) {
    background: var(--hover);
    color: var(--fg);
  }
  .rail button:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .rail button.on {
    background: var(--accent-bg);
    color: var(--accent);
  }
  /* A notch against the rail's edge, so the picked view reads even without colour. */
  .rail button.on::before {
    content: '';
    position: absolute;
    left: -0.3125rem;
    top: 0.5rem;
    bottom: 0.5rem;
    width: 3px;
    border-radius: 2px;
    background: var(--accent);
  }
  .rail .grow {
    flex: 1;
  }
  .rail .dot {
    margin-bottom: 0.375rem;
  }
  .rail .avatar {
    width: 1.75rem;
    height: 1.75rem;
    border-radius: 50%;
    background: var(--accent-bg);
    color: var(--accent);
    font: inherit;
    font-size: 0.75rem;
    font-weight: 600;
  }
  .rail .avatar:hover {
    background: var(--accent);
    color: var(--accent-fg);
  }
  .panel-head {
    display: flex;
    align-items: center;
    min-height: 2.5rem;
    box-sizing: border-box;
    padding: 0.25rem 0.75rem;
    border-bottom: 1px solid var(--border);
  }
  .panel-head h2 {
    margin: 0;
    font-size: 0.8125rem;
    font-weight: 600;
  }
  .denied {
    font-size: 0.8rem;
    background: #fee2e2;
    color: #991b1b;
    padding: 0.4rem 0.6rem;
    border-top: 1px solid #fca5a5;
  }
  .status {
    font-size: 0.75rem;
    color: var(--muted);
    padding: 0.3rem 0.6rem;
    border-top: 1px solid var(--border);
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: #22c55e;
    flex: none;
  }
  .offline .dot,
  .dot.offline {
    background: #f59e0b;
  }
  /* Panes sit side by side; each one manages its own tabs, editor and backlinks. */
  .main {
    grid-area: main;
    display: flex;
    min-width: 0;
    min-height: 0;
  }
  /* One-pixel divider between neighbouring panes (the class lives in Pane.svelte). */
  .main > :global(.pane + .pane) {
    border-left: 1px solid var(--border);
  }
  .link {
    font: inherit;
    border: 0;
    background: none;
    color: var(--accent);
    cursor: pointer;
    padding: 0;
  }
  .bookmarks-pane,
  .shared {
    display: flex;
    flex-direction: column;
    padding: 0.3rem;
    overflow: auto;
  }
  .shared {
    border-top: 1px solid var(--border);
    padding-top: 0.4rem;
  }
  .shared p {
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    margin: 0 0 0.2rem 0.5rem;
  }
  .bookmarks-pane button,
  .shared button {
    font: inherit;
    font-size: 0.9rem;
    text-align: left;
    border: 0;
    background: none;
    color: inherit;
    padding: 0.25rem 0.5rem;
    border-radius: 4px;
    cursor: pointer;
  }
  .bookmarks-pane button:hover,
  .shared button:hover {
    background: var(--hover);
  }
  .vault-tag {
    color: var(--muted);
    font-size: 0.75em;
    text-transform: uppercase;
    letter-spacing: 0.03em;
    margin-left: 0.4em;
  }
  .pad {
    padding: 0.6rem;
    font-size: 0.85rem;
  }

  /* ---- the narrow shell (see `NARROW` in lib/media.svelte.ts for the matching breakpoint)

     The sidebar leaves the grid entirely and becomes a fixed drawer, so opening it costs a
     transform rather than a re-layout of the editor underneath it. */
  .topbar {
    display: none;
    align-items: center;
    gap: 0.4rem;
    padding: 0.25rem 0.4rem;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
  }
  .topbar .icon {
    display: grid;
    place-items: center;
    font: inherit;
    font-size: 1.05rem;
    line-height: 1;
    border: 0;
    background: none;
    color: var(--muted);
    padding: 0.45rem 0.6rem;
    border-radius: 6px;
    cursor: pointer;
  }
  /* Only there when a pane is hidden: the count says so, and a tap lists them. */
  .topbar .panes {
    flex: none;
    display: flex;
    align-items: center;
    gap: 0.3rem;
    height: 2rem;
    padding: 0 0.55rem;
    border: 1px solid var(--border);
    border-radius: 1rem;
    background: var(--bg);
    color: var(--muted);
    font: inherit;
    font-size: 0.8rem;
    font-variant-numeric: tabular-nums;
    cursor: pointer;
  }
  .topbar .here {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.9rem;
    font-weight: 600;
  }
  /* The drawer and its scrim sit *under* every overlay (dialogs at 10, modals at 20): the
     palette and the prompts are opened from inside the drawer, and above it they were drawn
     behind it — visible through the scrim, never reachable. */
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 8;
    background: rgb(0 0 0 / 0.35);
  }
  /* Nothing but the panes: the sidebar stays in the window the tab came from. */
  .layout.detached,
  .layout.detached.narrow {
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: auto minmax(0, 1fr);
    grid-template-areas: 'denied' 'main';
  }
  .layout.detached > .denied {
    grid-area: denied;
  }
  /* `minmax(0, …)`, not a bare `1fr`: that one's minimum is the content's min-content width,
     and the top bar's includes the whole unwrapped note title — a long one widened the column
     past the screen and the page scrolled sideways. */
  .layout.narrow {
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: auto minmax(0, 1fr);
    grid-template-areas: 'top' 'main';
  }
  .layout.narrow .topbar {
    display: flex;
    grid-area: top;
  }
  .layout.narrow .vsplit {
    display: none;
  }
  .layout.narrow aside {
    position: fixed;
    inset: 0 auto 0 0;
    z-index: 9;
    width: min(21rem, 86vw);
    border-right: 1px solid var(--border);
    transform: translateX(-100%);
    transition: transform 0.18s ease;
  }
  .layout.narrow aside.open {
    transform: none;
    /* Only once it is out: a shadow on the parked drawer bleeds along the left edge. */
    box-shadow: 0 0 40px rgb(0 0 0 / 0.35);
  }
  /* The top bar has the daily note already, one tap from the note without the drawer. */
  .layout.narrow .rail .daily {
    display: none;
  }
  @media (prefers-reduced-motion: reduce) {
    .layout.narrow aside {
      transition: none;
    }
  }
  /* One pane at a time. The rest keep their state; they are simply not drawn, and the accent
     rule that says which pane has focus has nothing left to distinguish. */
  .layout.narrow .main > :global(.pane:not(.focused)) {
    display: none;
  }
  .layout.narrow .main > :global(.pane.focused) {
    border-top-color: transparent;
  }

  /* ---- touch: no hover to reveal anything, and a finger is not a pixel */
  @media (pointer: coarse) {
    .rail {
      width: 3.5rem;
      gap: 0.375rem;
    }
    .rail button {
      width: 2.75rem;
      height: 2.75rem;
    }
    .rail .avatar {
      width: 2rem;
      height: 2rem;
    }
    .panel-head {
      min-height: 3rem;
    }
    .bookmarks-pane button,
    .shared button {
      padding: 0.5rem;
    }
  }
</style>
