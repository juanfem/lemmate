// Link rewriting on rename (SPEC §4.4); mirrors `notes_core::markdown::rewrite_wikilinks`.

function base(path: string): string {
  const i = path.lastIndexOf('/')
  return i === -1 ? path : path.slice(i + 1)
}

/** Same rules as `notes_core::markdown::rewrite_wikilinks`. */
export function rewriteWikilinks(text: string, oldPath: string, newPath: string): string | null {
  const strip = (p: string) => p.replace(/\.(md|qmd)$/u, '')
  const oldStem = strip(oldPath)
  const newStem = strip(newPath)
  const oldBase = base(oldStem)
  const newBase = base(newStem)
  let changed = false
  const out = text.replace(/\[\[([^\]]*)\]\]/gu, (whole, inner: string) => {
    const k = inner.search(/[#|]/u)
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

