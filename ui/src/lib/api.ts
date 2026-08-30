// REST client for what the CRDT stream does not carry: search, backlinks, tags, vault list.

export interface VaultInfo {
  id: string
  notes: number
}
export interface NoteSummary {
  id: string
  path: string
  title: string | null
}
export interface SearchHit {
  note_id: string
  title: string | null
  snippet: string
}

async function get<T>(path: string): Promise<T> {
  const r = await fetch(`/api/v1${path}`)
  if (!r.ok) throw new Error(`${r.status} ${r.statusText} for ${path}`)
  return (await r.json()) as T
}

export const api = {
  vaults: () => get<VaultInfo[]>('/vaults'),
  notes: (vault: string) => get<NoteSummary[]>(`/vaults/${vault}/notes`),
  search: (vault: string, q: string, limit = 20) =>
    get<SearchHit[]>(`/vaults/${vault}/search?q=${encodeURIComponent(q)}&limit=${limit}`),
  backlinks: (vault: string, id: string) => get<NoteSummary[]>(`/vaults/${vault}/notes/${id}/backlinks`),
  tags: (vault: string) => get<{ tag: string; count: number }[]>(`/vaults/${vault}/tags`),
  attachmentUrl: (vault: string, hash: string) => `/api/v1/vaults/${vault}/attachments/${hash}`,
}
