// Where to go after signing in, when the sign-in was on the way somewhere (`?next=`).

/**
 * The page a sign-in should return to, from the location's query: a render opened in a browser
 * that had no session (the server sends it here with `?next=`). Only a render of this site is
 * followed — a path, never an address elsewhere — so the parameter cannot be used to send a
 * freshly signed-in user anywhere else. `null` for anything else.
 */
export function renderReturn(search: string): string | null {
  const next = new URLSearchParams(search).get('next')
  if (!next || !next.startsWith('/api/v1/vaults/') || next.includes('\\')) return null
  const path = next.split('?')[0]!
  if (!/^\/api\/v1\/vaults\/[0-9A-Z]{26}\/notes\/[0-9A-Z]{26}\/render(\/[0-9A-Z]{26})?$/u.test(path)) return null
  return next
}

/** A native app asking to be signed in (`crates/server/src/apps.rs`), from `?authorize=app&…`. */
export interface AppAuthorize {
  redirect_uri: string
  state: string
  code_challenge: string
  device: string
}

/**
 * The app sign-in request in the location's query, or null. The redirect must be `http://` on a
 * loopback address — the server refuses anything else anyway; checking here keeps the page from
 * offering to approve what would only fail.
 */
export function appAuthorize(search: string): AppAuthorize | null {
  const q = new URLSearchParams(search)
  if (q.get('authorize') !== 'app') return null
  const [redirect_uri, state, code_challenge, device] = ['redirect_uri', 'state', 'code_challenge', 'device'].map((k) => q.get(k) ?? '')
  let url: URL
  try {
    url = new URL(redirect_uri ?? '')
  } catch {
    return null
  }
  const loopback = ['127.0.0.1', '[::1]', 'localhost'].includes(url.hostname)
  if (url.protocol !== 'http:' || !loopback || !state || !code_challenge || !device) return null
  return { redirect_uri: redirect_uri!, state: state!, code_challenge: code_challenge!, device: device! }
}
