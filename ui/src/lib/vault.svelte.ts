// Live view of one vault: the vault doc (paths, attachments, bookmarks) and a cache of open
// note docs, all synced through one SyncClient. Reactive via Svelte 5 runes.

import * as Y from 'yjs'
import type { Awareness } from 'y-protocols/awareness'
import { SyncClient, type SyncStatus } from './sync.ts'
import { ulid } from './ulid.ts'

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
  readonly vaultDoc = new Y.Doc()
  private notesMap: Y.Map<string>
  private attachmentsMap: Y.Map<string>
  private bookmarksArr: Y.Array<Bookmark>
  private open = new Map<string, { doc: Y.Doc; awareness: Awareness; refs: number }>()

  notes: NoteEntry[] = $state([])
  attachments: Record<string, string> = $state({})
  bookmarks: Bookmark[] = $state([])
  status: SyncStatus = $state('connecting')
  vaultSynced = $state(false)

  /** Without the vault doc: for notes shared directly (SPEC §11.2), which grant only the note. */
  readonly noteOnly: boolean

  constructor(id: string, opts: { noteOnly?: boolean; wsUrl?: string } = {}) {
    this.id = id
    this.noteOnly = opts.noteOnly ?? false
    const wsUrl = opts.wsUrl ?? SyncClient.wsUrl()
    this.client = new SyncClient(wsUrl)
    this.client.onStatus = (s) => (this.status = s)
    this.client.onSynced = (docId) => {
      if (docId === this.vaultDocId) this.vaultSynced = true
    }
    this.notesMap = this.vaultDoc.getMap<string>('notes')
    this.attachmentsMap = this.vaultDoc.getMap<string>('attachments')
    this.bookmarksArr = this.vaultDoc.getArray<Bookmark>('bookmarks')
    const refresh = () => {
      this.notes = [...this.notesMap.entries()].map(([id, path]) => ({ id, path })).sort((a, b) => a.path.localeCompare(b.path))
      this.attachments = Object.fromEntries(this.attachmentsMap.entries())
      this.bookmarks = this.bookmarksArr.toArray()
    }
    this.notesMap.observe(refresh)
    this.attachmentsMap.observe(refresh)
    this.bookmarksArr.observe(refresh)
    if (!this.noteOnly) this.client.open(this.vaultDocId, this.vaultDoc)
    else this.vaultSynced = true
  }

  get vaultDocId() {
    return `vault:${this.id}`
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

  renameNote(id: string, path: string) {
    if (this.notesMap.get(id) !== path) this.notesMap.set(id, path)
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
    this.client.destroy()
    this.vaultDoc.destroy()
  }
}

// BLAKE3 is what the engine and server key attachments by. Browsers have no native BLAKE3;
// the tiny pure-JS implementation in ./blake3.ts is used for uploads only.
import { blake3Hex } from './blake3.ts'

export function basename(path: string): string {
  const i = path.lastIndexOf('/')
  return i === -1 ? path : path.slice(i + 1)
}

export function displayName(path: string): string {
  return basename(path).replace(/\.(md|qmd)$/u, '')
}
