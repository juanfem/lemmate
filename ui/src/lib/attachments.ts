import { api } from './api.ts'
import type { VaultSession } from './vault.svelte.ts'

/**
 * Where an `![[embed]]` points. A wiki-style target is a name, not a path, so it is tried
 * relative to the note first, then from the vault root, then in `attachments/`, and finally by
 * bare filename — the order a vault written by hand tends to mean.
 */
export function embedUrlFor(session: VaultSession, notePath: string, target: string): string | undefined {
  const t = target.trim()
  const dir = notePath.includes('/') ? notePath.slice(0, notePath.lastIndexOf('/') + 1) : ''
  const name = t.split('/').pop() ?? t
  for (const candidate of [dir + t, t, `attachments/${name}`]) {
    const hash = session.attachments[candidate]
    if (hash) return api.attachmentUrl(session.id, hash)
  }
  const byName = Object.entries(session.attachments).find(([p]) => p.split('/').pop() === name)
  return byName ? api.attachmentUrl(session.id, byName[1]) : undefined
}
