// Link rewriting on rename (SPEC §4.4); mirrors `lemmate_core::markdown::rewrite_wikilinks`.
// And the other way round: what a new link to a note should say.

function base(path: string): string {
  const i = path.lastIndexOf('/')
  return i === -1 ? path : path.slice(i + 1)
}

const strip = (p: string) => p.replace(/\.(md|qmd)$/u, '')
const folderOf = (p: string) => {
  const i = p.lastIndexOf('/')
  return i === -1 ? '' : p.slice(0, i)
}
const segments = (dir: string) => dir.split('/').filter((s) => s !== '')

/**
 * Which of the vault's note `paths` the wikilink `target`, written in the note at `from`, reaches
 * (SPEC §5.4) — `lemmate_core::markdown::resolve_wikilink`, which backlinks use, rule for rule.
 * An exact path wins, with or without its extension. Otherwise a bare name reaches the note of
 * that name, and where several share it, the one nearest `from`: the most folders in common,
 * then the shallowest, then the first by path. A target with a folder in it is a path only.
 */
export function resolveWikilink(target: string, from: string, paths: readonly string[]): string | undefined {
  const t = target.trim()
  const exact = /\.(md|qmd)$/u.test(t) ? [t] : [t, `${t}.md`, `${t}.qmd`]
  for (const want of exact) if (paths.includes(want)) return want
  if (t.includes('/')) return undefined
  const name = strip(t)
  const here = segments(folderOf(from))
  const shared = (p: string) => {
    const dir = segments(folderOf(p))
    let n = 0
    while (n < dir.length && n < here.length && dir[n] === here[n]) n++
    return n
  }
  const depth = (p: string) => p.split('/').length
  let best: string | undefined
  for (const p of paths) {
    if (strip(base(p)) !== name) continue
    if (best === undefined) best = p
    else {
      const by = shared(best) - shared(p) || depth(p) - depth(best) || (p < best ? -1 : 1)
      if (by < 0) best = p
    }
  }
  return best
}

/** Whether a bare name, in the note a rename rewrites, reached the renamed note before and still does. */
export interface BareName {
  before: boolean
  after: boolean
}

/** `lemmate_core::markdown::BareName::of`: from the note at `from`, with `paths` holding the note at `newPath`. */
export function bareName(from: string, oldPath: string, newPath: string, paths: readonly string[]): BareName {
  const before = paths.map((p) => (p === newPath ? oldPath : p))
  return {
    before: resolveWikilink(strip(base(oldPath)), from, before) === oldPath,
    after: resolveWikilink(strip(base(newPath)), from, paths) === newPath,
  }
}

/** Same rules as `lemmate_core::markdown::rewrite_wikilinks`. */
export function rewriteWikilinks(
  text: string,
  oldPath: string,
  newPath: string,
  bare: BareName = { before: true, after: true },
): string | null {
  const oldStem = strip(oldPath)
  const newStem = strip(newPath)
  const oldBase = base(oldStem)
  const newBase = base(newStem)
  let changed = false
  const out = text.replace(/\[\[([^\]]*)\]\]/gu, (whole, inner: string) => {
    // Inside a table the alias pipe is escaped, `[[Plan\|label]]`; the `\` goes with the suffix.
    const k = inner.search(/#|\\?\|/u)
    const target = (k === -1 ? inner : inner.slice(0, k)).trim()
    const suffix = k === -1 ? '' : inner.slice(k)
    let r: string | null = null
    if (target === oldPath || target === oldStem) r = newStem
    else if (target === oldBase && bare.before) r = !bare.after ? newStem : oldBase !== newBase ? newBase : null
    if (r === null) return whole
    changed = true
    return `[[${r}${suffix}]]`
  })
  return changed ? out : null
}


/**
 * What `[[…]]` should name to reach the note at `path`, among the vault's `paths`: the note's
 * name alone while no other note shares it, and its path (extension off) once one does — a bare
 * name resolves to whichever note comes first, and SPEC §5.4 wants an ambiguous one qualified.
 */
export function wikilinkTarget(path: string, paths: Iterable<string>): string {
  const strip = (p: string) => p.replace(/\.(md|qmd)$/u, '')
  const stem = strip(path)
  const name = base(stem)
  for (const p of paths) if (p !== path && base(strip(p)) === name) return stem
  return name
}
