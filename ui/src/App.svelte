<script lang="ts">
  import { onDestroy, untrack } from 'svelte'
  import { api, authState, type User } from './lib/api.ts'
  import Login from './components/Login.svelte'
  import AccountDialog from './components/AccountDialog.svelte'
  import Setup from './components/Setup.svelte'
  import { VaultSession, displayName } from './lib/vault.svelte.ts'
  import { Workspace } from './lib/workspace.svelte.ts'
  import { ulid } from './lib/ulid.ts'
  import FilesPane from './components/FilesPane.svelte'
  import type { VaultNode } from './lib/tree.ts'
  import { clamp, dragResize } from './lib/resize.ts'
  import Pane, { type PaneState } from './components/Pane.svelte'
  import QuickSwitcher from './components/QuickSwitcher.svelte'
  import SearchPane from './components/SearchPane.svelte'
  import TagsPane from './components/TagsPane.svelte'
  import OutlinePane, { type OutlineItem } from './components/OutlinePane.svelte'
  import CommandPalette, { type Command } from './components/CommandPalette.svelte'
  import HistoryPane from './components/HistoryPane.svelte'
  import TrashPane from './components/TrashPane.svelte'
  import ShareDialog from './components/ShareDialog.svelte'
  import SharedView from './components/SharedView.svelte'
  import ImportDialog from './components/ImportDialog.svelte'
  import type { SharedNote } from './lib/api.ts'
  import Modal from './components/Modal.svelte'

  // ---- first run (desktop): the relay serves the UI in setup mode until configured
  let setup = $state<{ config_path: string; suggested_vault_dir: string } | null>(null)
  let setupStarting = $state(false)
  if (!readPublicToken())
    fetch('/api/v1/local/setup')
      .then((r) => (r.ok ? r.json() : null))
      .then((j: { configured?: boolean; config_path?: string; suggested_vault_dir?: string } | null) => {
        if (j && j.configured === false) setup = { config_path: j.config_path ?? '', suggested_vault_dir: j.suggested_vault_dir ?? '' }
      })
      .catch(() => {})

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
    authRequired = false
    // The invite is spent now; leaving it in the URL would only re-show the register form.
    if (invite) {
      invite = null
      location.hash = ''
    }
    me = await api.me().catch(() => null)
    workspace?.refresh()
  }
  async function signOut() {
    await api.logout().catch(() => {})
    me = null
    location.hash = ''
    authRequired = true
  }

  // ---- the workspace: every vault at once (SPEC §9), on one socket
  //
  // Routes: `#/v/<vault>` focuses a vault, `#/v/<vault>/<note>` a note inside it, and both are
  // written back as you move around. `#/n/<vault>/<note>` (a note shared directly with you) and
  // `#/s/<token>` (a public link) are single-note views with no workspace behind them.
  const ULID = '[0-9A-HJKMNP-TV-Z]{26}'
  let workspace = $state<Workspace | null>(null)
  /** The single-note session behind `#/n/…`; never a workspace member. */
  let solo = $state<VaultSession | null>(null)
  /** Which vault the sidebar and "new note" act on when no note is focused. */
  let focusVault = $state<string | null>(null)
  let routeNote = $state<string | null>(readRouteNote())

  function readRouteVault(): string | null {
    const m = new RegExp(`^#/v/(${ULID})`, 'u').exec(location.hash)
    if (m) return m[1]!
    const n = new RegExp(`^#/n/(${ULID})/(${ULID})`, 'u').exec(location.hash)
    return n ? n[1]! : null
  }
  function readRouteNote(): string | null {
    const m = new RegExp(`^#/v/${ULID}/(${ULID})`, 'u').exec(location.hash)
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
    if (n && n !== active) routeNote = n
  })
  let sharedWithMe: SharedNote[] = $state([])
  let shareOpen = $state(false)
  let accountOpen = $state(false)
  let importInto = $state<string | null | undefined>(undefined)

  // The single-note view stands alone: one session, one pane, its own socket.
  $effect(() => {
    const only = noteOnly
    const vault = readRouteVault()
    untrack(() => {
      solo?.destroy()
      solo = only && vault ? new VaultSession(vault, { noteOnly: true }) : null
      if (solo && only) {
        panes = [{ id: ++paneSeq, tabs: [only], active: only }]
        focusedPane = 0
        pinned = []
        closed = []
      }
    })
  })

  // Everything else runs in the workspace, created once and kept for the session.
  $effect(() => {
    if (publicToken || noteOnly || workspace) return
    untrack(() => {
      const ws = new Workspace()
      workspace = ws
      // Debug/automation handle (used by scripts/cdp.mjs smoke runs).
      ;(window as unknown as { lemmate?: unknown }).lemmate = { workspace: ws }
      const restored = loadLayout()
      panes = restored.panes
      focusedPane = restored.focused
      pinned = loadPinned()
      focusVault = readRouteVault()
      ws.refresh().then((vaults) => {
        if (!focusVault && vaults.length) focusVault = vaults[0]!.id
      })
      api.sharedWithMe().then((s) => (sharedWithMe = s)).catch(() => (sharedWithMe = []))
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
    switcher = true
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
  let panes: PaneState[] = $state([blankPane()])
  let focusedPane = $state(0)
  /** Recently closed note ids, most recent last (Ctrl+Shift+T reopens). */
  let closed: string[] = $state([])
  let pinned: string[] = $state([])
  let headingsByPane: Record<number, OutlineItem[]> = $state({})
  let presenceByPane: Record<number, string[]> = $state({})
  let jumpers: Record<number, ((pos: number) => void) | undefined> = $state({})
  let layoutRestored = $state(false)

  let sidebar: 'files' | 'search' | 'tags' | 'outline' | 'bookmarks' | 'history' | 'trash' = $state('files')
  let switcher = $state(false)
  let palette = $state(false)
  let tagsVersion = $state(0)

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
    return { id: ++paneSeq, tabs: [], active: null }
  }
  let focused = $derived(panes[Math.min(focusedPane, panes.length - 1)] ?? panes[0]!)
  /** The focused pane's note: everything outside the panes (commands, sidebar) acts on it. */
  let active = $derived(focused.active)
  let headings = $derived(headingsByPane[focused.id] ?? [])
  let presence = $derived(presenceByPane[focused.id] ?? [])

  /** The session behind a note id, whichever vault holds it. */
  function sessionOf(noteId: string | null | undefined): VaultSession | undefined {
    if (solo) return solo
    return workspace?.sessionForNote(noteId) ?? undefined
  }
  /** What the sidebar acts on: the focused note's vault, else the one you last touched. */
  let session = $derived(solo ?? sessionOf(active) ?? workspace?.get(focusVault) ?? workspace?.sessions[0])
  let vaults: VaultNode[] = $derived(
    (workspace?.sessions ?? []).map((s) => ({ id: s.id, label: s.label, notes: s.notes })),
  )
  /** Vault labels are noise until there is more than one vault to tell apart. */
  let manyVaults = $derived((workspace?.sessions.length ?? 0) > 1)
  function vaultLabel(vault: string | null | undefined): string {
    return manyVaults && vault ? (workspace?.label(vault) ?? '') : ''
  }
  function labelOfNote(noteId: string): string {
    return vaultLabel(workspace?.vaultOfNote(noteId))
  }

  // The route follows the focused note, so a reload or a copied URL comes back to it.
  $effect(() => {
    if (publicToken || noteOnly || invite) return
    const vault = session?.id
    const note = active
    const want = note && vault ? `#/v/${vault}/${note}` : vault ? `#/v/${vault}` : ''
    if (want && location.hash !== want) location.hash = want
  })

  // ---- layout persistence, per device, across every vault
  interface StoredLayout {
    panes?: { tabs?: string[]; active?: string | null }[]
    focused?: number
  }
  function loadLayout(): { panes: PaneState[]; focused: number } {
    try {
      const raw = localStorage.getItem('lemmate.layout')
      const data = raw ? (JSON.parse(raw) as StoredLayout) : null
      const list = (data?.panes ?? [])
        .filter((p) => Array.isArray(p.tabs) && p.tabs.length > 0)
        .slice(0, MAX_PANES)
        .map((p) => {
          const tabs = (p.tabs ?? []).filter((t) => typeof t === 'string')
          return { id: ++paneSeq, tabs, active: p.active && tabs.includes(p.active) ? p.active : (tabs[0] ?? null) }
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
    if (solo || !workspace) return
    const data = JSON.stringify({ panes: panes.map((p) => ({ tabs: [...p.tabs], active: p.active })), focused: focusedPane })
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
      const kept = panes.map((p) => ({ ...p, tabs: p.tabs.filter((t) => known.has(t)) })).map((p) => ({ ...p, active: p.active && p.tabs.includes(p.active) ? p.active : (p.tabs[0] ?? null) }))
      const live = kept.filter((p) => p.tabs.length > 0)
      panes = live.length ? live : [blankPane()]
      focusedPane = Math.min(focusedPane, panes.length - 1)
      pinned = pinned.filter((id) => known.has(id))
    })
  })
  // A note named by the URL (a link someone sent, a reload) opens as soon as it is known.
  $effect(() => {
    const ws = workspace
    const want = routeNote
    if (!ws || !want) return
    if (!ws.notes.some((n) => n.id === want)) return
    untrack(() => {
      routeNote = null
      open(want)
    })
  })

  // ---- in-app prompt/confirm (native dialogs are unreliable in the Tauri webview)
  interface AskOptions {
    kind: 'prompt' | 'confirm'
    title: string
    initial?: string
    placeholder?: string
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

  /** Open a note in the focused pane. */
  function open(id: string) {
    const p = focused
    if (!p.tabs.includes(id)) p.tabs = [...p.tabs, id]
    p.active = id
    switcher = false
    palette = false
    headingsByPane[p.id] = []
    closed = closed.filter((c) => c !== id)
    const vault = workspace?.vaultOfNote(id)
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
    closed = [...closed.filter((c) => c !== id), id].slice(-20)
    if (p.tabs.length === 0 && panes.length > 1) closePane(panes.indexOf(p))
  }
  function splitRight() {
    if (solo || panes.length >= MAX_PANES) return
    const id = focused.active
    if (!id) return
    const i = panes.indexOf(focused)
    panes = [...panes.slice(0, i + 1), { id: ++paneSeq, tabs: [id], active: id }, ...panes.slice(i + 1)]
    focusedPane = i + 1
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
    if (id && sessionOf(id)?.pathOf(id)) open(id)
  }
  function togglePin(id: string) {
    pinned = pinned.includes(id) ? pinned.filter((p) => p !== id) : [...pinned, id]
    try {
      localStorage.setItem('lemmate.pins', JSON.stringify(pinned))
    } catch {
      /* storage may be unavailable */
    }
  }
  function bookmarkActive() {
    const s = sessionOf(active)
    if (!s || !active) return
    const path = s.pathOf(active)
    if (path) s.toggleBookmark({ kind: 'note', target: path, label: displayName(path) })
  }
  let commands: Command[] = $derived([
    { id: 'open', label: 'Open or create note…', shortcut: 'Ctrl+O', run: () => (switcher = true) },
    { id: 'daily', label: "Open today's daily note", shortcut: 'Ctrl+Shift+D', run: daily },
    { id: 'search', label: 'Search all vaults', shortcut: 'Ctrl+Shift+F', run: () => (sidebar = 'search') },
    { id: 'files', label: 'Show files', run: () => (sidebar = 'files') },
    { id: 'tags', label: 'Show tags', run: () => (sidebar = 'tags') },
    { id: 'outline', label: 'Show outline', run: () => (sidebar = 'outline') },
    { id: 'bookmarks', label: 'Show bookmarks', run: () => (sidebar = 'bookmarks') },
    { id: 'history', label: 'Show version history', run: () => (sidebar = 'history') },
    { id: 'trash', label: 'Show trash', run: () => (sidebar = 'trash') },
    { id: 'share', label: 'Share note…', run: () => (shareOpen = !!active) },
    { id: 'export-html', label: 'Export note as HTML', run: () => exportActive('html') },
    { id: 'export-docx', label: 'Export note as DOCX', run: () => exportActive('docx') },
    { id: 'export-pdf', label: 'Export note as PDF', run: () => exportActive('pdf') },
    { id: 'export-slides', label: 'Export note as slides (reveal.js)', run: () => exportActive('revealjs') },
    { id: 'bookmark', label: session && active && session.isBookmarked('note', session.pathOf(active) ?? '') ? 'Remove bookmark' : 'Bookmark this note', shortcut: 'Ctrl+Shift+B', run: bookmarkActive },
    { id: 'rename', label: 'Rename / move note', run: renameActive },
    { id: 'delete', label: 'Move note to trash', run: deleteActive },
    { id: 'close', label: 'Close tab', shortcut: 'Ctrl+W', run: () => active && close(active) },
    { id: 'pin', label: active && pinned.includes(active) ? 'Unpin tab' : 'Pin tab', run: () => active && togglePin(active) },
    { id: 'reopen', label: 'Reopen closed tab', shortcut: 'Ctrl+Shift+T', run: reopenClosed },
    { id: 'split', label: 'Split right', shortcut: 'Ctrl+\\', run: splitRight },
    { id: 'closepane', label: 'Close pane', run: () => closePane() },
    { id: 'nextpane', label: 'Focus next pane', shortcut: 'Ctrl+Alt+→', run: () => focusPane(1) },
    { id: 'newvault', label: 'New vault…', run: newVault },
    ...(session ? [{ id: 'renamevault', label: `Rename vault “${session.label}”…`, run: () => renameVault(session!.id) }] : []),
    { id: 'import', label: 'Import an Obsidian vault…', run: () => (importInto = session?.id ?? null) },
    ...(me && me.id !== 'local' ? [{ id: 'account', label: 'Account, password and invites…', run: () => (accountOpen = true) }] : []),
    ...(me && me.id !== 'local' ? [{ id: 'signout', label: `Sign out (${me.email})`, run: signOut }] : []),
  ])
  /** Templates (SPEC §9): `Templates/<name>.md` with {{date}}, {{date:FORMAT}}, {{time}}, {{title}}. */
  async function template(vault: VaultSession, name: string, title: string, fallback: string): Promise<string> {
    const id = vault.idOf(`Templates/${name}.md`)
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
      return fillTemplate(body, title)
    } finally {
      release()
    }
  }
  function fillTemplate(body: string, title: string): string {
    const d = new Date()
    const pad = (n: number) => String(n).padStart(2, '0')
    const fmt = (f: string) =>
      f
        .replace(/YYYY/gu, String(d.getFullYear()))
        .replace(/MM/gu, pad(d.getMonth() + 1))
        .replace(/DD/gu, pad(d.getDate()))
        .replace(/HH/gu, pad(d.getHours()))
        .replace(/mm/gu, pad(d.getMinutes()))
    return body
      .replace(/\{\{date:([^}]+)\}\}/gu, (_, f: string) => fmt(f))
      .replace(/\{\{date\}\}/gu, fmt('YYYY-MM-DD'))
      .replace(/\{\{time\}\}/gu, fmt('HH:mm'))
      .replace(/\{\{title\}\}/gu, title)
      .replace(/\{\{cursor\}\}/gu, '')
  }
  async function create(path: string, vault: string | undefined = session?.id) {
    const s = workspace?.get(vault) ?? (solo ? undefined : undefined)
    if (!s) return
    const title = displayName(path)
    open(s.createNote(path, await template(s, 'Note', title, `# ${title}\n\n`)))
  }
  async function daily() {
    const s = session
    if (!s) return
    const d = new Date()
    const name = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
    const path = `Daily/${name}.md`
    const existing = s.idOf(path)
    open(existing ?? s.createNote(path, await template(s, 'Daily', name, `# ${name}\n\n`)))
  }
  async function renameActive() {
    const s = sessionOf(active)
    if (!s || !active) return
    const id = active
    const current = s.pathOf(id) ?? ''
    const next = (await ask({ kind: 'prompt', title: 'Rename / move note', initial: current, placeholder: 'folder/note.md' }))?.trim()
    if (next && next !== current) s.renameNote(id, next.endsWith('.md') || next.endsWith('.qmd') ? next : `${next}.md`)
  }
  async function deleteActive() {
    const s = sessionOf(active)
    if (!s || !active) return
    const id = active
    const path = s.pathOf(id) ?? id
    const ok = await ask({ kind: 'confirm', title: `Move “${path}” to trash?`, confirmLabel: 'Move to trash', danger: true })
    if (ok === null) return
    s.deleteNote(id)
    // The note is gone: close it in every pane, pinned or not.
    for (const p of [...panes]) if (p.tabs.includes(id)) close(id, true)
    closed = closed.filter((c) => c !== id)
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
    const blob = await r.blob()
    const name = (r.headers.get('content-disposition')?.match(/filename="([^"]+)"/u)?.[1] ?? `${displayName(s.pathOf(id) ?? 'note')}.${format}`).replace(/[/\\]/gu, '-')
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = name
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
    const mod = e.ctrlKey || e.metaKey
    if (!mod) return
    if (e.altKey && (e.key === 'ArrowRight' || e.key === 'ArrowLeft')) {
      focusPane(e.key === 'ArrowRight' ? 1 : -1)
      e.preventDefault()
    } else if (e.key === '\\') {
      splitRight()
      e.preventDefault()
    } else if ((e.key === 'o' || e.key === 'p') && !e.shiftKey) {
      switcher = !switcher
      palette = false
      e.preventDefault()
    } else if (e.key === 'n' && !e.shiftKey) {
      switcher = true
      e.preventDefault()
    } else if (e.key === 'w') {
      if (active) close(active)
      e.preventDefault()
    } else if ((e.key === 't' || e.key === 'T') && e.shiftKey) {
      reopenClosed()
      e.preventDefault()
    } else if (e.key === 'd' && e.shiftKey) {
      daily()
      e.preventDefault()
    } else if (e.key === 'f' && e.shiftKey) {
      sidebar = 'search'
      e.preventDefault()
    } else if ((e.key === 'p' || e.key === 'P') && e.shiftKey) {
      palette = !palette
      switcher = false
      e.preventDefault()
    } else if ((e.key === 'b' || e.key === 'B') && e.shiftKey) {
      bookmarkActive()
      e.preventDefault()
    }
  }

  let activePath = $derived(session && active ? (session.pathOf(active) ?? (solo ? 'shared note' : '(deleted)')) : '')
  let denied = $derived(solo ? solo.denied : (workspace?.denied ?? null))
  let status = $derived(solo ? solo.status : (workspace?.status ?? 'connecting'))
  let noteCount = $derived(solo ? solo.notes.length : (workspace?.noteCount ?? 0))
  let syncing = $derived(solo ? !solo.vaultSynced : !(workspace?.synced ?? false))
  // Built here rather than in the markup: an `{#if}` in the middle of a sentence eats the
  // whitespace in front of it, and this line is all conditional pieces.
  let statusLine = $derived(
    [
      status + (status === 'online' && syncing ? ' · syncing…' : ''),
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
  <Login {invite} onDone={signedIn} />
{:else if !workspace && !solo}
  <main class="welcome"><h1>Lemmate</h1><p class="muted">Loading…</p></main>
{:else}
  <div class="layout" style:--side="{sideWidth}px">
    <aside>
      <div class="side-tabs">
        <button class:on={sidebar === 'files'} onclick={() => (sidebar = 'files')} title="Files">Files</button>
        <button class:on={sidebar === 'search'} onclick={() => (sidebar = 'search')} title="Search (Ctrl+Shift+F)">Search</button>
        <button class:on={sidebar === 'tags'} onclick={() => (sidebar = 'tags')} title="Tags">Tags</button>
        <button class:on={sidebar === 'outline'} onclick={() => (sidebar = 'outline')} title="Outline">Outline</button>
        <button class:on={sidebar === 'bookmarks'} onclick={() => (sidebar = 'bookmarks')} title="Bookmarks">★</button>
        <button class:on={sidebar === 'history'} onclick={() => (sidebar = 'history')} title="Version history">⏱</button>
        <span class="spacer"></span>
        <button title="New note (Ctrl+N)" onclick={() => (switcher = true)}>＋</button>
        <button title="Command palette (Ctrl+Shift+P)" onclick={() => (palette = true)}>⌘</button>
      </div>
      {#if solo}
        <p class="muted pad">A note shared with you. <button class="link" onclick={() => (location.hash = '')}>All vaults</button></p>
      {:else if sidebar === 'files'}
        <FilesPane
          {vaults}
          activeId={active}
          activeVault={session?.id ?? null}
          onOpen={open}
          actions={{
            onCreateIn: createInFolder,
            onRenameFolder: renameFolder,
            onDeleteFolder: deleteFolder,
            onCreateInVault: createInVault,
            onRenameVault: renameVault,
            onImportInto: (v) => (importInto = v),
            onNewVault: newVault,
          }}
        />
        {#if sharedWithMe.length}
          <nav class="shared">
            <p class="muted">Shared with me</p>
            {#each sharedWithMe as n (n.id)}
              <button onclick={() => (location.hash = `#/n/${n.vault_id}/${n.id}`)} title={n.path}>{n.title ?? displayName(n.path)}</button>
            {/each}
          </nav>
        {/if}
      {:else if sidebar === 'search'}
        <SearchPane label={labelOfNote} onOpen={open} />
      {:else if sidebar === 'tags'}
        {#if session}<TagsPane vault={session.id} version={tagsVersion} onOpen={open} />{/if}
      {:else if sidebar === 'outline'}
        <OutlinePane items={headings} onJump={(pos) => jumpers[focused.id]?.(pos)} />
      {:else if sidebar === 'trash'}
        {#if session}<TrashPane vault={session.id} version={tagsVersion} onRestored={(id) => open(id)} />{/if}
      {:else if sidebar === 'history'}
        {#if session}<HistoryPane {session} noteId={active} onAsk={(title, initial) => ask({ kind: 'prompt', title, initial })} />{/if}
      {:else}
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
      <footer class="status" class:offline={status !== 'online'}>
        <span class="dot"></span>
        {statusLine}
      </footer>
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
    <section class="main">
      {#each panes as p, i (p.id)}
        <Pane
          lookup={sessionOf}
          vaultLabel={labelOfNote}
          pane={p}
          focused={i === focusedPane}
          {pinned}
          onActivate={(id) => { focusedPane = i; open(id) }}
          onClose={(id) => { focusedPane = i; close(id) }}
          onFocus={() => (focusedPane = i)}
          onBookmark={bookmarkActive}
          onShare={() => (shareOpen = true)}
          onRename={renameActive}
          onDelete={deleteActive}
          onOpen={(id) => { focusedPane = i; open(id) }}
          onHeadings={(h) => (headingsByPane[p.id] = h)}
          onPresence={(names) => (presenceByPane[p.id] = names)}
          bind:jumpTo={jumpers[p.id]}
        />
      {/each}
    </section>
  </div>
  {#if switcher && workspace}
    <QuickSwitcher
      notes={workspace.notes}
      label={vaultLabel}
      createVault={session?.id ?? null}
      onOpen={open}
      onCreate={(path) => create(path)}
      onClose={() => (switcher = false)}
    />
  {/if}
  {#if palette}
    <CommandPalette {commands} onClose={() => (palette = false)} />
  {/if}
  {#if shareOpen && active && session}
    <ShareDialog vault={session.id} noteId={active} path={activePath} onClose={() => (shareOpen = false)} />
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
  <AccountDialog {me} onClose={() => (accountOpen = false)} />
{/if}

{#if modal}
  <Modal
    title={modal.title}
    kind={modal.kind}
    initial={modal.initial}
    placeholder={modal.placeholder}
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
    /* The divider draws the border between the two, so it can light up while you drag it. */
    grid-template-columns: var(--side, 17rem) auto 1fr;
    height: 100%;
  }
  aside {
    background: var(--panel);
    display: grid;
    grid-template-rows: auto minmax(0, 1fr) auto auto;
    min-height: 0;
    min-width: 0;
  }
  /* Grab area wider than the hairline it draws, so the drag is not a pixel hunt. */
  .vsplit {
    width: 7px;
    margin: 0 -3px;
    cursor: col-resize;
    position: relative;
    z-index: 1;
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
  .side-tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 0.2rem;
    padding: 0.4rem;
    border-bottom: 1px solid var(--border);
  }
  .side-tabs button {
    font: inherit;
    font-size: 0.85rem;
    border: 0;
    background: none;
    color: var(--muted);
    padding: 0.2rem 0.5rem;
    border-radius: 4px;
    cursor: pointer;
  }
  .side-tabs button.on {
    color: var(--fg);
    background: var(--hover);
  }
  .spacer {
    flex: 1;
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
  }
  .offline .dot {
    background: #f59e0b;
  }
  /* Panes sit side by side; each one manages its own tabs, editor and backlinks. */
  .main {
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
</style>
