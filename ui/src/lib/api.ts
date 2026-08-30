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

export const api = {
  me: () => get<User>('/auth/me'),
  login: (email: string, password: string) => post<{ token: string; user: User }>('/auth/login', { email, password, device: navigator.userAgent.slice(0, 40) }),
  register: (email: string, password: string, display_name: string) =>
    post<{ token?: string; user?: User }>('/auth/register', { email, password, display_name }),
  logout: () => post<null>('/auth/logout', {}),
  members: (vault: string) => get<{ user_id: string; email: string; display_name: string; role: string }[]>(`/vaults/${vault}/members`),
  setMember: async (vault: string, email: string, role: string) => {
    const r = await fetch(`/api/v1/vaults/${vault}/members`, { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ email, role }) })
    if (!r.ok) throw new ApiError(r.status, `${r.status} for members`)
  },
  removeMember: async (vault: string, userId: string) => {
    const r = await fetch(`/api/v1/vaults/${vault}/members/${userId}`, { method: 'DELETE' })
    if (!r.ok) throw new ApiError(r.status, `${r.status} for members`)
  },
  vaults: () => get<VaultInfo[]>('/vaults'),
  notes: (vault: string) => get<NoteSummary[]>(`/vaults/${vault}/notes`),
  search: (vault: string, q: string, limit = 20) =>
    get<SearchHit[]>(`/vaults/${vault}/search?q=${encodeURIComponent(q)}&limit=${limit}`),
  backlinks: (vault: string, id: string) => get<NoteSummary[]>(`/vaults/${vault}/notes/${id}/backlinks`),
  tags: (vault: string) => get<{ tag: string; count: number }[]>(`/vaults/${vault}/tags`),
  tagged: (vault: string, tag: string) => get<NoteSummary[]>(`/vaults/${vault}/tagged?tag=${encodeURIComponent(tag)}`),
  attachmentUrl: (vault: string, hash: string) => `/api/v1/vaults/${vault}/attachments/${hash}`,
}
