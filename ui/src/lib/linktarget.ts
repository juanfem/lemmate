// Where a markdown link (`[text](href)`) or a bare address goes when it is followed: out to the
// web, or to a note or a file of the vault, named by a path relative to the note it is in.

export type LinkTarget =
  /** An address with a scheme — `https:`, `mailto:`, … — for the browser. */
  | { kind: 'web'; url: string }
  /** A note, by its path in the vault (the extension may be left off, as in a wikilink). */
  | { kind: 'note'; path: string }
  /** Any other file of the vault, by its path. */
  | { kind: 'file'; path: string }

/** Schemes a note's link must not run: they execute rather than go anywhere. */
const UNSAFE = new Set(['javascript', 'vbscript', 'data', 'blob'])

/**
 * What `href`, written in the note at `notePath`, points at — undefined for nothing worth
 * following (an empty link, a bare `#heading`, a script). A relative path is relative to the
 * note's folder, a leading `/` to the vault root; `..` never climbs out of the vault.
 */
export function linkTarget(notePath: string, href: string): LinkTarget | undefined {
  let h = href.trim()
  // CommonMark lets a destination with spaces be written `<like this>`.
  if (h.startsWith('<') && h.endsWith('>')) h = h.slice(1, -1).trim()
  const scheme = /^([a-z][a-z0-9+.-]*):/iu.exec(h)?.[1]?.toLowerCase()
  // `C:\…` is a drive letter, not a scheme; no note links there either.
  if (scheme && scheme.length > 1) return UNSAFE.has(scheme) ? undefined : { kind: 'web', url: h }
  if (scheme) return undefined
  h = h.replace(/[?#].*$/su, '')
  if (!h) return undefined
  try {
    h = decodeURI(h)
  } catch {
    // A stray `%` is just a character in the name.
  }
  const base = h.startsWith('/') ? [] : notePath.split('/').slice(0, -1)
  const parts = [...base]
  for (const seg of h.split('/')) {
    if (seg === '' || seg === '.') continue
    if (seg === '..') parts.pop()
    else parts.push(seg)
  }
  const path = parts.join('/')
  if (!path) return undefined
  const name = parts[parts.length - 1]!
  const ext = name.includes('.') ? name.slice(name.lastIndexOf('.') + 1).toLowerCase() : ''
  return ext === '' || ext === 'md' || ext === 'qmd' ? { kind: 'note', path } : { kind: 'file', path }
}

/** Follow `href` if it is a web address — for views with no vault to find a relative one in. */
export function openWebLink(href: string) {
  const target = linkTarget('', href)
  if (target?.kind === 'web') window.open(target.url, '_blank', 'noopener,noreferrer')
}

/** The one same-origin address a note's image may load: an attachment, by vault and hash. */
const ATTACHMENT_PATH = /^\/api\/v1\/vaults\/[^/]+\/attachments\/[^/]+$/u

/**
 * Whether the browser may fetch `url` for an image in a note, and as what. A note's author
 * picks the address and a reader's browser fetches it with the reader's cookies, so an image
 * pointed at the app's own API is a request made in the reader's name — a GET with an effect
 * (opening a daily note creates it), or an expensive one, run by anyone who opens the note.
 * Of our own origin only attachments load; other sites' images and `data:image/…` do as before.
 * Undefined: show nothing.
 */
export function imageSrc(url: string, origin: string = globalThis.location?.origin ?? 'http://localhost'): string | undefined {
  let u: URL
  try {
    u = new URL(url.trim(), `${origin}/`)
  } catch {
    return undefined
  }
  if (u.protocol === 'data:') return /^data:image\//iu.test(u.href) ? u.href : undefined
  if (u.protocol !== 'http:' && u.protocol !== 'https:') return undefined
  if (u.origin !== origin) return u.href
  return ATTACHMENT_PATH.test(u.pathname) ? u.href : undefined
}
