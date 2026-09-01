// REST client for what the CRDT stream does not carry: search, backlinks, tags, vault list.

export interface VaultInfo {
  id: string
  notes: number
}
export interface NoteSummary {
  id: string
  path: string
  title: string | null
  /** When the server last saw this note change. Sent on the listing only (SPEC §6.4). */
  updated_at?: string
}
export interface Share {
  kind: 'user' | 'link'
  user_id: string | null
  email: string | null
  role: string
  expires_ms: number | null
  link: string | null
}
export interface SharedNote {
  id: string
  vault_id: string
  path: string
  title: string | null
  role: string
}
export interface Version {
  seq: number
  created_ms: number
  label: string | null
  author: string | null
}
export interface SearchHit {
  note_id: string
  title: string | null
  snippet: string
}

/** What one import upload did (SPEC §11.4). A whole import is several requests, so the UI adds
 * these up as the batches complete. */
export interface ImportReport {
  notes: number
  attachments: number
  callouts: number
  embeds: number
  skipped: number
  bookmarks: number
  daily_notes: boolean
}

export interface Invite {
  id: string
  created_ms: number
  expires_ms: number | null
  used_ms: number | null
  used_by: string | null
  usable: boolean
  /** The registration URL, returned only by the call that mints the invite. */
  link: string | null
}

export interface User {
  id: string
  email: string
  display_name: string
  is_admin: boolean
}

/** Set when any call comes back 401; the shell shows the login screen. */
export const authState = { onUnauthorized: () => {} }

export class ApiError extends Error {
  status: number
  constructor(status: number, message: string) {
    super(message)
    this.status = status
  }
}

async function get<T>(path: string): Promise<T> {
  const r = await fetch(`/api/v1${path}`)
  if (r.status === 401) authState.onUnauthorized()
  if (!r.ok) throw new ApiError(r.status, `${r.status} ${r.statusText} for ${path}`)
  return (await r.json()) as T
}

async function post<T>(path: string, body: unknown): Promise<T> {
  const r = await fetch(`/api/v1${path}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) })
  if (!r.ok) throw new ApiError(r.status, `${r.status} ${r.statusText} for ${path}`)
  const text = await r.text()
  return (text ? JSON.parse(text) : null) as T
}

async function del(path: string): Promise<void> {
  const r = await fetch(`/api/v1${path}`, { method: 'DELETE' })
  if (r.status === 401) authState.onUnauthorized()
  if (!r.ok) throw new ApiError(r.status, `${r.status} ${r.statusText} for ${path}`)
}

export const api = {
  me: () => get<User>('/auth/me'),
  login: (email: string, password: string) => post<{ token: string; user: User }>('/auth/login', { email, password, device: navigator.userAgent.slice(0, 40) }),
  register: (email: string, password: string, display_name: string, invite?: string) =>
    post<{ token?: string; user?: User }>('/auth/register', { email, password, display_name, invite }),
  logout: () => post<null>('/auth/logout', {}),
  /** Own password (send `current`), or an admin resetting `email` (do not). */
  changePassword: (new_password: string, current?: string, email?: string) =>
    post<{ sessions_revoked: number }>('/auth/password', { new_password, current_password: current, email }),
  invites: () => get<Invite[]>('/invites'),
  createInvite: (expires_days?: number) => post<Invite>('/invites', { expires_days }),
  revokeInvite: (id: string) => del(`/invites/${id}`),
  members: (vault: string) => get<{ user_id: string; email: string; display_name: string; role: string }[]>(`/vaults/${vault}/members`),
  setMember: async (vault: string, email: string, role: string) => {
    const r = await fetch(`/api/v1/vaults/${vault}/members`, { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ email, role }) })
    if (!r.ok) throw new ApiError(r.status, `${r.status} for members`)
  },
  removeMember: async (vault: string, userId: string) => {
    const r = await fetch(`/api/v1/vaults/${vault}/members/${userId}`, { method: 'DELETE' })
    if (!r.ok) throw new ApiError(r.status, `${r.status} for members`)
  },
  shares: (vault: string, id: string) => get<Share[]>(`/vaults/${vault}/notes/${id}/shares`),
  share: async (vault: string, id: string, body: { kind: 'user' | 'link'; email?: string; role?: string; expires_days?: number }) => {
    const r = await fetch(`/api/v1/vaults/${vault}/notes/${id}/shares`, { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) })
    if (!r.ok) throw new ApiError(r.status, `${r.status} for share`)
    return (await r.json()) as Share
  },
  unshare: async (vault: string, id: string, body: { user_id?: string; links?: boolean }) => {
    const r = await fetch(`/api/v1/vaults/${vault}/notes/${id}/shares`, { method: 'DELETE', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) })
    if (!r.ok) throw new ApiError(r.status, `${r.status} for unshare`)
  },
  sharedWithMe: () => get<SharedNote[]>('/shared-with-me'),
  publicNote: (token: string) => get<{ id: string; path: string; title: string | null; content: string }>(`/shared/${token}`),
  trash: (vault: string) => get<{ id: string; path: string; title: string | null; deleted_at: string }[]>(`/vaults/${vault}/trash`),
  restore: (vault: string, id: string) => post<NoteSummary>(`/vaults/${vault}/notes/${id}/restore`, {}),
  versions: (vault: string, id: string) => get<Version[]>(`/vaults/${vault}/notes/${id}/versions`),
  versionAt: (vault: string, id: string, seq: number) => get<{ seq: number; content: string }>(`/vaults/${vault}/notes/${id}/versions/${seq}`),
  saveVersion: (vault: string, id: string, label: string) => post<Version>(`/vaults/${vault}/notes/${id}/versions`, { label }),
  vaults: () => get<VaultInfo[]>('/vaults'),
  notes: (vault: string) => get<NoteSummary[]>(`/vaults/${vault}/notes`),
  search: (vault: string, q: string, limit = 20) =>
    get<SearchHit[]>(`/vaults/${vault}/search?q=${encodeURIComponent(q)}&limit=${limit}`),
  /** Every vault the account can read, ranked together (the relay answers for its one vault). */
  searchAll: (q: string, limit = 30) => get<SearchHit[]>(`/search?q=${encodeURIComponent(q)}&limit=${limit}`),
  backlinks: (vault: string, id: string) => get<NoteSummary[]>(`/vaults/${vault}/notes/${id}/backlinks`),
  tags: (vault: string) => get<{ tag: string; count: number }[]>(`/vaults/${vault}/tags`),
  tagged: (vault: string, tag: string) => get<NoteSummary[]>(`/vaults/${vault}/tagged?tag=${encodeURIComponent(tag)}`),
  attachmentUrl: (vault: string, hash: string) => `/api/v1/vaults/${vault}/attachments/${hash}`,
  /**
   * One batch of an Obsidian import (SPEC §11.4): the files are sent as multipart parts named
   * by their vault-relative path, and the server (or the relay) converts them. Splitting a big
   * vault across requests is safe — a path that already exists is skipped, not duplicated.
   */
  importBatch: async (vault: string, files: { path: string; file: File }[]): Promise<ImportReport> => {
    const form = new FormData()
    for (const f of files) form.append('file', f.file, f.path)
    const r = await fetch(`/api/v1/vaults/${vault}/import`, { method: 'POST', body: form })
    if (r.status === 401) authState.onUnauthorized()
    if (!r.ok) throw new ApiError(r.status, `${r.status} ${r.statusText} for import`)
    return (await r.json()) as ImportReport
  },
}
