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

export class VaultSession {
  readonly id: string
  readonly client: SyncClient
  readonly vaultDoc = new Y.Doc()
  private notesMap: Y.Map<string>
  private attachmentsMap: Y.Map<string>
  private open = new Map<string, { doc: Y.Doc; awareness: Awareness; refs: number }>()

  notes: NoteEntry[] = $state([])
  attachments: Record<string, string> = $state({})
  status: SyncStatus = $state('connecting')
  vaultSynced = $state(false)

  constructor(id: string, wsUrl: string = SyncClient.wsUrl()) {
    this.id = id
    this.client = new SyncClient(wsUrl)
    this.client.onStatus = (s) => (this.status = s)
    this.client.onSynced = (docId) => {
      if (docId === this.vaultDocId) this.vaultSynced = true
    }
    this.notesMap = this.vaultDoc.getMap<string>('notes')
    this.attachmentsMap = this.vaultDoc.getMap<string>('attachments')
    const refresh = () => {
      this.notes = [...this.notesMap.entries()].map(([id, path]) => ({ id, path })).sort((a, b) => a.path.localeCompare(b.path))
      this.attachments = Object.fromEntries(this.attachmentsMap.entries())
    }
    this.notesMap.observe(refresh)
    this.attachmentsMap.observe(refresh)
    this.client.open(this.vaultDocId, this.vaultDoc)
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

  destroy() {
    this.client.destroy()
    this.vaultDoc.destroy()
  }
}

export function basename(path: string): string {
  const i = path.lastIndexOf('/')
  return i === -1 ? path : path.slice(i + 1)
}

export function displayName(path: string): string {
  return basename(path).replace(/\.(md|qmd)$/u, '')
}
