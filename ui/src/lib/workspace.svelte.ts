// Every vault you can see, live at once (SPEC §4.3, §9): one WebSocket, one `VaultSession` per
// vault, and the flat views the shell needs across all of them — the tree's roots, the quick
// switcher's list, the note → vault lookup that lets a tab, a search hit or a link name a note
// without naming its vault. Note ids are ULIDs and unique across vaults, so everything outside
// this module keeps addressing notes by id alone.

import { api, type VaultInfo } from './api.ts'
import { SyncClient, type SyncStatus } from './sync.ts'
import { VaultSession, type Bookmark, type NoteEntry } from './vault.svelte.ts'
import { uniquePath } from './moves.ts'

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

  /**
   * Move notes onto `dest` in `toVault` — the file browser's drag-and-drop and its context
   * menu both land here.
   *
   * Inside one vault this is a rename per note, so `[[links]]` follow (SPEC §4.4). Across
   * vaults it cannot be: a note id is an entry in one vault doc. The note is re-created in the
   * target vault with a fresh id, the attachments it references are copied over, and the
   * original is moved to trash — which is why the caller confirms first, and why links from
   * notes left behind in the source vault are reported as broken rather than rewritten.
   */
  async moveNotes(
    moves: { id: string; from: string; to: string }[],
    fromVault: string,
    toVault: string,
  ): Promise<{ moved: { id: string; path: string }[]; failed: { path: string; error: string }[] }> {
    const src = this.get(fromVault)
    const dst = this.get(toVault)
    const moved: { id: string; path: string }[] = []
    const failed: { path: string; error: string }[] = []
    if (!src || !dst) return { moved, failed }
    if (fromVault === toVault) {
      for (const m of moves) {
        try {
          await src.renameNote(m.id, m.to)
          moved.push({ id: m.id, path: m.to })
        } catch (e) {
          failed.push({ path: m.from, error: String(e) })
        }
      }
      return { moved, failed }
    }
    // Cross-vault: one pass so names claimed earlier in this batch are seen by later ones.
    const taken = new Set(dst.notes.map((n) => n.path))
    for (const m of moves) {
      try {
        const body = await src.noteText(m.id)
        const path = uniquePath(m.to, taken)
        taken.add(path)
        const id = dst.adoptNote(path, body)
        await this.copyAttachments(src, dst, body)
        src.deleteNote(m.id)
        moved.push({ id, path })
      } catch (e) {
        failed.push({ path: m.from, error: String(e) })
      }
    }
    return { moved, failed }
  }

  /**
   * Carry over every attachment the note references. Content-addressed on both sides, so a
   * blob the target vault already holds costs one hash and no upload; one that fails to copy
   * leaves the reference in place rather than failing the move.
   */
  private async copyAttachments(src: VaultSession, dst: VaultSession, body: string) {
    const referenced = new Set<string>()
    for (const m of body.matchAll(/!\[\[([^\]]+)\]\]/gu)) referenced.add(m[1]!.split('|')[0]!.trim())
    for (const m of body.matchAll(/!\[[^\]]*\]\(([^)\s]+)\)/gu)) referenced.add(m[1]!.trim())
    for (const ref of referenced) {
      const name = ref.split('/').pop() ?? ref
      const entry =
        Object.entries(src.attachments).find(([p]) => p === ref) ??
        Object.entries(src.attachments).find(([p]) => p.split('/').pop() === name)
      if (!entry) continue
      const [, hash] = entry
      if (Object.values(dst.attachments).includes(hash)) continue
      try {
        await dst.uploadAttachment(name, await src.attachmentBytes(hash), '')
      } catch {
        /* the reference survives; the blob can be re-uploaded by hand */
      }
    }
  }

  destroy() {
    for (const s of this.sessions) s.destroy()
    this.sessions = []
    this.client.destroy()
  }
}
