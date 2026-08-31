// Every vault you can see, live at once (SPEC §4.3, §9): one WebSocket, one `VaultSession` per
// vault, and the flat views the shell needs across all of them — the tree's roots, the quick
// switcher's list, the note → vault lookup that lets a tab, a search hit or a link name a note
// without naming its vault. Note ids are ULIDs and unique across vaults, so everything outside
// this module keeps addressing notes by id alone.

import { api, type VaultInfo } from './api.ts'
import { SyncClient, type SyncStatus } from './sync.ts'
import { VaultSession, type Bookmark, type NoteEntry } from './vault.svelte.ts'

export interface WorkspaceNote extends NoteEntry {
  vault: string
}

export interface WorkspaceBookmark extends Bookmark {
  vault: string
}

export class Workspace {
  readonly client: SyncClient
  sessions: VaultSession[] = $state([])
  status: SyncStatus = $state('connecting')
  /** The vault list has been fetched at least once (an empty list then means "no vaults"). */
  listed = $state(false)
  /** Last permission denial from the server, whichever vault it came from. */
  denied: { docId: string; reason: string } | null = $state(null)

  constructor(opts: { wsUrl?: string } = {}) {
    this.client = new SyncClient(opts.wsUrl ?? SyncClient.wsUrl())
    this.status = this.client.status
    this.client.onStatus = (s) => (this.status = s)
    this.client.onSynced = (docId) => this.sessions.find((s) => s.handlesDoc(docId))?.onSynced(docId)
    this.client.onDenied = (docId, reason) => {
      this.denied = { docId, reason }
      this.sessions.find((s) => s.handlesDoc(docId))?.onDenied(docId, reason)
    }
  }

  /** Fetch the vault list and open a session for every vault, dropping any that went away. */
  async refresh(): Promise<VaultInfo[]> {
    let vaults: VaultInfo[] = []
    try {
      vaults = await api.vaults()
    } catch {
      return []
    }
    this.listed = true
    const wanted = new Set(vaults.map((v) => v.id))
    for (const s of this.sessions) if (!wanted.has(s.id)) s.destroy()
    const kept = this.sessions.filter((s) => wanted.has(s.id))
    const known = new Set(kept.map((s) => s.id))
    const added = vaults.filter((v) => !known.has(v.id)).map((v) => new VaultSession(v.id, { client: this.client }))
    this.sessions = [...kept, ...added]
    return vaults
  }

  /** Open a vault that is not in the list yet — a brand-new one, or one joined by id. */
  add(id: string): VaultSession {
    const existing = this.get(id)
    if (existing) return existing
    const session = new VaultSession(id, { client: this.client })
    this.sessions = [...this.sessions, session]
    return session
  }

  get(id: string | null | undefined): VaultSession | undefined {
    return id ? this.sessions.find((s) => s.id === id) : undefined
  }

  /** Which vault holds this note? Answerable once that vault's doc has synced. */
  sessionForNote(noteId: string | null | undefined): VaultSession | undefined {
    return noteId ? this.sessions.find((s) => s.notes.some((n) => n.id === noteId)) : undefined
  }

  vaultOfNote(noteId: string | null | undefined): string | undefined {
    return this.sessionForNote(noteId)?.id
  }

  pathOf(noteId: string): string | undefined {
    return this.sessionForNote(noteId)?.pathOf(noteId)
  }

  /** Every note in every vault, for the quick switcher and for restoring tabs. */
  notes: WorkspaceNote[] = $derived(
    this.sessions.flatMap((s) => s.notes.map((n) => ({ ...n, vault: s.id }))),
  )

  /** Every bookmark in every vault, labelled with the vault it belongs to. */
  bookmarks: WorkspaceBookmark[] = $derived(
    this.sessions.flatMap((s) => s.bookmarks.map((b) => ({ ...b, vault: s.id }))),
  )

  noteCount = $derived(this.sessions.reduce((n, s) => n + s.notes.length, 0))
  /** Every vault doc has caught up, so "this note no longer exists" is a safe conclusion. */
  synced = $derived(this.sessions.length > 0 && this.sessions.every((s) => s.vaultSynced))

  label(vault: string): string {
    return this.get(vault)?.label ?? `vault ${vault.slice(-6)}`
  }

  destroy() {
    for (const s of this.sessions) s.destroy()
    this.sessions = []
    this.client.destroy()
  }
}
