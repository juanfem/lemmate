// Link rewriting on rename (SPEC §4.4); mirrors `lemmate_core::markdown::rewrite_wikilinks`.
// And the other way round: what a new link to a note should say.

function base(path: string): string {
  const i = path.lastIndexOf('/')
  return i === -1 ? path : path.slice(i + 1)
}

/** Same rules as `lemmate_core::markdown::rewrite_wikilinks`. */
export function rewriteWikilinks(text: string, oldPath: string, newPath: string): string | null {
  const strip = (p: string) => p.replace(/\.(md|qmd)$/u, '')
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
    else if (target === oldBase && oldBase !== newBase) r = newBase
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
