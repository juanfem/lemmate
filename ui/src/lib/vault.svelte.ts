// Live view of one vault: the vault doc (paths, attachments, bookmarks, name) and a cache of
// open note docs. Reactive via Svelte 5 runes.
//
// The socket is not per vault: `Workspace` (workspace.svelte.ts) hands every session the same
// SyncClient, because the frame protocol is addressed by doc id and one connection carries as
// many vaults as it likes. A session created without one (a directly shared note, a public
// link) owns its client and tears it down with itself.

import * as Y from 'yjs'
import type { Awareness } from 'y-protocols/awareness'
import { IndexeddbPersistence } from 'y-indexeddb'
import { api } from './api.ts'
import { loadIds, loadMap, saveIds, saveMap } from './idstore.ts'
import { isInstalled } from './install.ts'

/** Breathing room between background note fetches, so they stay behind the foreground. */
const PREFETCH_GAP_MS = 50
/** How often an installed client re-checks the server for notes that changed elsewhere. */
const REFRESH_EVERY_MS = 60_000
import { SyncClient, type SyncStatus } from './sync.ts'
import { rewriteWikilinks } from './links.ts'
export { rewriteWikilinks }
import { ulid } from './ulid.ts'
import { restampId } from './moves.ts'

export interface NoteEntry {
  id: string
  path: string
}

/** SPEC §4.3: bookmarks live in the vault doc so every replica shares them. */
export interface Bookmark {
  kind: 'note' | 'folder' | 'search' | 'heading'
  target: string
  label: string
}

export class VaultSession {
  readonly id: string
  readonly client: SyncClient
  /** Whether `destroy()` should take the socket down with it. */
  private readonly ownsClient: boolean
  readonly vaultDoc = new Y.Doc()
  private notesMap: Y.Map<string>
  private attachmentsMap: Y.Map<string>
  private bookmarksArr: Y.Array<Bookmark>
  private metaMap: Y.Map<string>
  private open = new Map<
    string,
    { doc: Y.Doc; awareness: Awareness; refs: number; store: IndexeddbPersistence | null }
  >()
  /**
   * Notes edited here that the server has not acknowledged (see lib/idstore.ts). A note doc is
   * only pushed while it is subscribed on the socket, so an edit made offline in a note that is
   * then closed had nowhere to go: reconnecting re-handshook the docs still open and left that
   * one sitting in IndexedDB until someone happened to open it again. These stay subscribed
   * until the server says it has them, and are re-subscribed on the next start if the tab went
   * away first.
   */
  private pending: Set<string>
  private readonly pendingKey: string
  /**
   * The version of each note whose content has been pulled for offline reading: note id to the
   * server's `updated_at` at the time it was fetched.
   *
   * A note doc only reaches IndexedDB once it has been subscribed, so without this only notes
   * someone had opened were readable with the network down — the vault's file tree was complete
   * and almost none of it would open. Holding the version rather than a bare "have it" also
   * makes the copies *stay* right: a note edited on another device shows a newer `updated_at`
   * in the listing, and gets fetched again.
   */
  private cached: Map<string, string>
  private readonly cachedKey: string
  private stocking = false
  private refreshTimer: ReturnType<typeof setInterval> | null = null
  private onVisible: (() => void) | null = null
  /** Waiters for a doc's first successful sync, resolved from `onSynced`. */
  private syncWaiters = new Map<string, (() => void)[]>()

  notes: NoteEntry[] = $state([])
  attachments: Record<string, string> = $state({})
  bookmarks: Bookmark[] = $state([])
  /** Optional display name, shared by every replica; falls back to a short id in the UI. */
  name = $state('')
  status: SyncStatus = $state('connecting')
  vaultSynced = $state(false)
  /** Last permission denial from the server, for the shell to show. */
  denied: { docId: string; reason: string } | null = $state(null)

  /** Without the vault doc: for notes shared directly (SPEC §11.2), which grant only the note. */
  readonly noteOnly: boolean

  constructor(id: string, opts: { noteOnly?: boolean; wsUrl?: string; client?: SyncClient } = {}) {
    this.id = id
    this.noteOnly = opts.noteOnly ?? false
    this.pendingKey = `lemmate.pending.${id}`
    this.pending = loadIds(this.pendingKey)
    this.cachedKey = `lemmate.cached.${id}`
    this.cached = loadMap(this.cachedKey)
    this.ownsClient = !opts.client
    this.client = opts.client ?? new SyncClient(opts.wsUrl ?? SyncClient.wsUrl())
    if (this.ownsClient) {
      // A shared client is driven by the workspace, which fans these out to every session.
      this.client.onStatus = (s) => (this.status = s)
      this.client.onSynced = (docId) => this.onSynced(docId)
      this.client.onDenied = (docId, reason) => this.onDenied(docId, reason)
    }
    this.notesMap = this.vaultDoc.getMap<string>('notes')
    this.attachmentsMap = this.vaultDoc.getMap<string>('attachments')
    this.bookmarksArr = this.vaultDoc.getArray<Bookmark>('bookmarks')
    this.metaMap = this.vaultDoc.getMap<string>('meta')
    const refresh = () => {
      this.notes = [...this.notesMap.entries()].map(([id, path]) => ({ id, path })).sort((a, b) => a.path.localeCompare(b.path))
      this.attachments = Object.fromEntries(this.attachmentsMap.entries())
      this.bookmarks = this.bookmarksArr.toArray()
      this.name = this.metaMap.get('name') ?? ''
    }
    this.notesMap.observe(refresh)
    this.attachmentsMap.observe(refresh)
    this.bookmarksArr.observe(refresh)
    this.metaMap.observe(refresh)
    if (!this.noteOnly) {
      this.cache(this.vaultDocId, this.vaultDoc)
      this.client.open(this.vaultDocId, this.vaultDoc)
      // Whatever was owed to the server when the tab last closed is still owed. Subscribing it
      // again now means the next connection pushes it, rather than waiting for someone to
      // happen to open that note.
      for (const id of this.pending) this.acquire(id).release()
    } else this.vaultSynced = true
  }

  /**
   * Offline cache (SPEC §6.4): docs opened here stay readable/editable after a reload.
   *
   * The instance is handed back because it is also an update *origin*: hydrating a doc from
   * IndexedDB looks exactly like an edit unless you can tell the two apart, and mistaking one
   * for the other would mark every note you open as needing a push.
   */
  private cache(docId: string, doc: Y.Doc): IndexeddbPersistence | null {
    try {
      return new IndexeddbPersistence(`lemmate:${this.id}:${docId}`, doc)
    } catch {
      /* private mode or no IndexedDB: online-only */
      return null
    }
  }

  get vaultDocId() {
    return `vault:${this.id}`
  }

  /** Does this session own `docId`? The workspace routes shared-socket events by this. */
  handlesDoc(docId: string): boolean {
    return docId === this.vaultDocId || this.open.has(docId)
  }

  onSynced(docId: string) {
    if (docId === this.vaultDocId) {
      this.vaultSynced = true
      // The note list is complete now, so the contents can be filled in behind it. This also
      // fires on every reconnect, which is exactly when a refresh is due.
      this.startRefreshing()
      void this.stock()
    }
    for (const resolve of this.syncWaiters.get(docId) ?? []) resolve()
    this.syncWaiters.delete(docId)
    // The server has this note now, so it no longer has to be held open on our account.
    if (this.pending.delete(docId)) {
      saveIds(this.pendingKey, this.pending)
      const entry = this.open.get(docId)
      if (entry && entry.refs <= 0) this.drop(docId)
    }
  }

  /** Resolve when the socket reports `docId` synced, or after `ms` — offline is an answer. */
  private whenSynced(docId: string, ms = 5000): Promise<void> {
    if (this.client.isSynced(docId)) return Promise.resolve()
    return new Promise<void>((resolve) => {
      const waiters = this.syncWaiters.get(docId) ?? []
      waiters.push(resolve)
      this.syncWaiters.set(docId, waiters)
      setTimeout(resolve, ms)
    })
  }

  /** Note this doc as needing a push, and keep it subscribed until it gets one. */
  private markPending(docId: string) {
    if (this.pending.has(docId)) return
    this.pending.add(docId)
    saveIds(this.pendingKey, this.pending)
  }

  /**
   * Keep the offline copies complete and current: fetch the content of notes nobody has open,
   * and re-fetch any whose `updated_at` has moved since we stored them (SPEC §3.2, §6.4).
   *
   * Only when the app is installed. In a browser tab this would be a copy of every note left
   * behind on whatever machine someone happened to sign in from, paid for with their bandwidth,
   * to enable an offline mode a tab does not really have. See lib/install.ts.
   *
   * The note you are reading is not this method's business: it is subscribed on the socket for
   * as long as it is open and updates the instant anyone else touches it. This is for the other
   * few hundred, and it runs one at a time with a gap, behind whatever the user is doing. It
   * gives up the moment the socket drops and resumes on the next pass.
   */
  private async stock() {
    if (this.stocking || this.noteOnly || !isInstalled()) return
    this.stocking = true
    try {
      // One listing answers "what exists" and "what changed" together, in a request far
      // cheaper than opening the docs to find out.
      const listed = await api.notes(this.id).catch(() => null)
      if (!listed) return
      for (const note of listed) {
        if (this.client.status !== 'online') break
        // No version from the server means nothing to compare, so treat having it as enough.
        const want = note.updated_at ?? this.cached.get(note.id) ?? ''
        if (this.cached.get(note.id) === want) continue
        // An open note is already live; re-syncing it here would fight with its editor.
        if (this.open.has(note.id)) {
          this.cached.set(note.id, want)
          continue
        }
        const { release } = this.acquire(note.id)
        try {
          await this.whenSynced(note.id)
        } finally {
          release()
        }
        this.cached.set(note.id, want)
        saveMap(this.cachedKey, this.cached)
        await new Promise((resolve) => setTimeout(resolve, PREFETCH_GAP_MS))
      }
      // Notes deleted elsewhere should stop taking up room in the record.
      const alive = new Set(listed.map((n) => n.id))
      for (const id of [...this.cached.keys()]) if (!alive.has(id)) this.cached.delete(id)
      saveMap(this.cachedKey, this.cached)
    } finally {
      this.stocking = false
    }
  }

  /**
   * Run `stock` on the three occasions that matter, once the vault doc has synced: now, every
   * minute, and whenever the app comes back to the foreground — which on a phone is the moment
   * someone opens it expecting to find what they wrote on the other device.
   */
  private startRefreshing() {
    if (this.refreshTimer !== null || this.noteOnly || !isInstalled()) return
    this.refreshTimer = setInterval(() => void this.stock(), REFRESH_EVERY_MS)
    this.onVisible = () => {
      if (document.visibilityState === 'visible') void this.stock()
    }
    document.addEventListener('visibilitychange', this.onVisible)
  }

  /** Unsubscribe and forget a note doc. Only ever called when nothing is waiting on it. */
  private drop(docId: string) {
    const entry = this.open.get(docId)
    if (!entry) return
    this.client.close(docId)
    entry.doc.destroy()
    this.open.delete(docId)
  }

  onDenied(docId: string, reason: string) {
    this.denied = { docId, reason }
  }

  /** Rename the vault for everyone (SPEC §4.3): the label lives in the vault doc. */
  setName(name: string) {
    const trimmed = name.trim()
    if (trimmed) this.metaMap.set('name', trimmed)
    else this.metaMap.delete('name')
  }

  /** What to call this vault in the UI when it has no name of its own. */
  get label(): string {
    return this.name || `vault ${this.id.slice(-6)}`
  }

  pathOf(id: string): string | undefined {
    return this.notesMap.get(id)
  }

  idOf(path: string): string | undefined {
    for (const [id, p] of this.notesMap) if (p === path) return id
    return undefined
  }

  /** Resolve a wikilink target the way the engine does: exact path, basename, then `.md`. */
  resolveLink(target: string): NoteEntry | undefined {
    const t = target.trim()
    const withExt = t.endsWith('.md') || t.endsWith('.qmd') ? t : `${t}.md`
    return (
      this.notes.find((n) => n.path === withExt || n.path === t) ??
      this.notes.find((n) => basename(n.path).replace(/\.(md|qmd)$/u, '') === basename(t).replace(/\.(md|qmd)$/u, ''))
    )
  }

  /** Open (or share) a note doc; call `release` when the view goes away. */
  acquire(id: string): { doc: Y.Doc; awareness: Awareness; release: () => void } {
    let e = this.open.get(id)
    if (!e) {
      const doc = new Y.Doc()
      const store = this.cache(id, doc)
      const awareness = this.client.open(id, doc)
      e = { doc, awareness, refs: 0, store }
      // An update from neither the socket nor the local cache was made here, and has to reach
      // the server before this doc may be dropped.
      doc.on('update', (_update: Uint8Array, origin: unknown) => {
        if (origin === this.client || (store !== null && origin === store)) return
        this.markPending(id)
      })
      this.open.set(id, e)
    }
    e.refs++
    const entry = e
    return {
      doc: entry.doc,
      awareness: entry.awareness,
      release: () => {
        entry.refs--
        // Unsent edits outrank the view that made them: keep the subscription until the
        // server acknowledges, and `onSynced` will drop it then.
        if (entry.refs <= 0 && !this.pending.has(id)) this.drop(id)
      },
    }
  }

  /** Create a note at `path` with front matter carrying its id (SPEC §6.3). */
  createNote(path: string, body = ''): string {
    const id = ulid()
    const { doc, release } = this.acquire(id)
    doc.getText('content').insert(0, `---\nid: ${id}\n---\n${body}`)
    this.notesMap.set(id, path)
    release()
    return id
  }

  /**
   * Create a note from another note's full markdown, re-stamping the front-matter id — a
   * copy into a second vault is a different note, and two of them may not share an id.
   */
  adoptNote(path: string, text: string): string {
    const id = ulid()
    const { doc, release } = this.acquire(id)
    doc.getText('content').insert(0, restampId(text, id))
    this.notesMap.set(id, path)
    release()
    return id
  }

  /** Rename/move, then rewrite `[[links]]` in referring notes (SPEC §4.4). */
  async renameNote(id: string, path: string) {
    const old = this.notesMap.get(id)
    if (old === path) return
    this.notesMap.set(id, path)
    if (!old) return
    let referrers: { id: string }[] = []
    try {
      referrers = await (await fetch(`/api/v1/vaults/${this.id}/notes/${id}/backlinks`)).json()
    } catch {
      return
    }
    for (const r of referrers) {
      if (r.id === id) continue
      const { doc, release } = this.acquire(r.id)
      try {
        await this.whenLoaded(r.id, doc)
        const text = doc.getText('content')
        const fixed = rewriteWikilinks(text.toString(), old, path)
        if (fixed !== null) doc.transact(() => (text.delete(0, text.length), text.insert(0, fixed)))
      } finally {
        release()
      }
    }
  }

  /**
   * Resolve once a note doc has content to read: either the socket says it is synced, or the
   * first update lands. The timeout is the offline case — an empty doc is still an answer.
   */
  private whenLoaded(id: string, doc: Y.Doc): Promise<void> {
    return new Promise<void>((resolve) => {
      if (this.client.isSynced(id)) return resolve()
      const t = setTimeout(resolve, 3000)
      const once = () => (clearTimeout(t), doc.getText('content').unobserve(once), resolve())
      doc.getText('content').observe(once)
    })
  }

  /** A note's markdown, once it has loaded. For moving one to another vault (SPEC §4.3). */
  async noteText(id: string): Promise<string> {
    const { doc, release } = this.acquire(id)
    try {
      await this.whenLoaded(id, doc)
      return doc.getText('content').toString()
    } finally {
      release()
    }
  }

  /** Fetch an attachment's bytes, to hand to another vault's `uploadAttachment`. */
  async attachmentBytes(hash: string): Promise<Uint8Array> {
    const r = await fetch(`/api/v1/vaults/${this.id}/attachments/${hash}`)
    if (!r.ok) throw new Error(`attachment ${hash.slice(0, 8)}: ${r.status}`)
    return new Uint8Array(await r.arrayBuffer())
  }

  deleteNote(id: string) {
    this.notesMap.delete(id)
  }

  toggleBookmark(b: Bookmark) {
    const i = this.bookmarksArr.toArray().findIndex((x) => x.kind === b.kind && x.target === b.target)
    if (i >= 0) this.bookmarksArr.delete(i, 1)
    else this.bookmarksArr.push([b])
  }

  isBookmarked(kind: Bookmark['kind'], target: string): boolean {
    return this.bookmarks.some((x) => x.kind === kind && x.target === target)
  }

  /**
   * Upload bytes as an attachment and return the vault-relative path to reference. Against
   * the relay the engine files it under `attachments/`; against the server we choose the
   * path and record it in the vault doc ourselves (SPEC §7: upload before referencing).
   */
  async uploadAttachment(name: string, bytes: Uint8Array, mime: string): Promise<string> {
    const hash = await blake3Hex(bytes)
    const res = await fetch(`/api/v1/vaults/${this.id}/attachments/${hash}`, {
      method: 'PUT',
      headers: { 'content-type': mime || 'application/octet-stream', 'x-filename': name },
      body: bytes as unknown as BodyInit,
    })
    if (!res.ok) throw new Error(`upload failed: ${res.status}`)
    const text = await res.text()
    if (text.trim().startsWith('{')) {
      const stored = JSON.parse(text) as { path: string; hash: string }
      return stored.path
    }
    let path = `attachments/${name}`
    if (this.attachmentsMap.get(path) && this.attachmentsMap.get(path) !== hash) {
      const dot = name.lastIndexOf('.')
      path = dot > 0 ? `attachments/${name.slice(0, dot)}-${hash.slice(0, 6)}${name.slice(dot)}` : `attachments/${name}-${hash.slice(0, 6)}`
    }
    if (this.attachmentsMap.get(path) !== hash) this.attachmentsMap.set(path, hash)
    return path
  }

  destroy() {
    if (this.refreshTimer !== null) clearInterval(this.refreshTimer)
    this.refreshTimer = null
    if (this.onVisible) document.removeEventListener('visibilitychange', this.onVisible)
    this.onVisible = null
    if (this.ownsClient) {
      this.client.destroy()
    } else {
      // The socket outlives us: close only the docs this session opened on it.
      for (const id of [...this.open.keys()]) this.client.close(id)
      if (!this.noteOnly) this.client.close(this.vaultDocId)
    }
    for (const e of this.open.values()) e.doc.destroy()
    this.open.clear()
    this.vaultDoc.destroy()
  }
}

// BLAKE3 is what the engine and server key attachments by. Browsers have no native BLAKE3;
// the tiny pure-JS implementation in ./blake3.ts is used for uploads only.
import { blake3Hex } from './blake3.ts'
import { basename } from './tree.ts'
export { basename }


export function displayName(path: string): string {
  return basename(path).replace(/\.(md|qmd)$/u, '')
}
