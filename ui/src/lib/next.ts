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
