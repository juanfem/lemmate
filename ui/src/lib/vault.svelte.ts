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
  private open = new Map<string, { doc: Y.Doc; awareness: Awareness; refs: number }>()

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
    } else this.vaultSynced = true
  }

  /** Offline cache (SPEC §6.4): docs opened here stay readable/editable after a reload. */
  private cache(docId: string, doc: Y.Doc) {
    try {
      new IndexeddbPersistence(`lemmate:${this.id}:${docId}`, doc)
    } catch {
      /* private mode or no IndexedDB: online-only */
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
    if (docId === this.vaultDocId) this.vaultSynced = true
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
      this.cache(id, doc)
      const awareness = this.client.open(id, doc)
      e = { doc, awareness, refs: 0 }
      this.open.set(id, e)
    }
    e.refs++
    const entry = e
    return {
      doc: entry.doc,
      awareness: entry.awareness,
      release: () => {
        entry.refs--
        if (entry.refs <= 0) {
          this.client.close(id)
          entry.doc.destroy()
          this.open.delete(id)
        }
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
