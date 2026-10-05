// The front-matter half of the indexer, split out from `index.ts` so that a caller who only
// wants a note's declared tags — the editor's tag shelf does, on every keystroke — pays for the
// YAML parser and not for micromark and its four mdast extensions as well.
//
// This is still the twin of `crates/core/src/markdown.rs`; `test/corpus.test.ts` covers it
// through `index()`.

import { isAlias, isMap, isScalar, parseDocument, type Document } from 'yaml'

export interface FrontMatter {
  title: string | null
  tags: string[]
  aliases: string[]
  id: string | null
}

export const EMPTY_FM: FrontMatter = { title: null, tags: [], aliases: [], id: null }

/**
 * Mirrors serde's strictness: any field of the wrong shape voids the whole front matter. As
 * there, a scalar `title`/`id` is its text as written — `title: 2024` is "2024", `id: 1.50` is
 * "1.50" — while `tags` and `aliases` must be strings.
 */
export function parseFrontMatter(src: string): FrontMatter {
  let doc: Document
  try {
    doc = parseDocument(src)
  } catch {
    return { ...EMPTY_FM }
  }
  if (doc.errors.length > 0 || !isMap(doc.contents)) return { ...EMPTY_FM }
  const scalar = (key: string): string | null => {
    let node: unknown = doc.get(key, true)
    if (isAlias(node)) node = node.resolve(doc)
    if (node === undefined || node === null) return null
    if (!isScalar(node)) throw new TypeError('expected string')
    if (node.value === null || node.value === undefined) return null
    if (typeof node.value === 'string') return node.value
    return node.source ?? String(node.value)
  }
  try {
    const o = doc.toJS() as Record<string, unknown>
    return { title: scalar('title'), tags: oneOrMany(o.tags), aliases: oneOrMany(o.aliases), id: scalar('id') }
  } catch {
    return { ...EMPTY_FM }
  }
}

/**
 * The leading `---` block of a note, parsed, without walking the body. The delimiters are the
 * ones micromark's frontmatter extension accepts, so a block this finds is a block `index()`
 * would have found too.
 */
export function frontMatter(source: string): FrontMatter {
  const m = /^---[ \t]*\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n|$)/u.exec(source)
  return m ? parseFrontMatter(m[1] ?? '') : { ...EMPTY_FM }
}

/** Add one tag the way the index does: trimmed, unslashed, lower-cased, first mention wins. */
export function pushTag(tags: string[], tag: string): void {
  const t = tag.trim().replace(/^\/+/u, '').replace(/\/+$/u, '').toLowerCase()
  if (t.length > 0 && !tags.includes(t)) tags.push(t)
}

function oneOrMany(v: unknown): string[] {
  if (v === null || v === undefined) return []
  if (typeof v === 'string')
    return v
      .split(',')
      .map((t) => t.trim())
      .filter((t) => t.length > 0)
  if (Array.isArray(v) && v.every((x) => typeof x === 'string')) return v as string[]
  throw new TypeError('expected string or list of strings')
}
