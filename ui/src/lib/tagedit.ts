// Writing a tag into a note. A tag the reader *adds* is a declaration about the note rather
// than a word in its prose, so it goes in the front matter — the same place `tags:` already
// reads from, and the place it can be removed from again without hunting through the text.
//
// This is text surgery on YAML written by hand, so it meets the note where it is: whichever of
// the three list shapes the note already uses, a new tag joins it rather than converting it.

/** The characters a tag may be made of — the same set `parseTag` accepts for an inline `#tag`. */
const TAG_CHAR = /[\p{Alphabetic}\p{N}_\-/]/u

/**
 * YAML scalars that are *not* strings, in either 1.1 or 1.2. A tag list holding one of these
 * comes back as a bool or a number, which voids the whole front matter (both indexers demand a
 * list of strings), so those get quoted. `no` is a plausible tag; a front matter it silently
 * deletes the title from is not.
 */
const RESERVED = /^(y|n|yes|no|true|false|on|off|null)$/iu

/** Bare where it is unambiguously a string, quoted where YAML would read it as something else. */
function scalar(tag: string): string {
  return /^\p{Alphabetic}[\p{Alphabetic}\p{N}_\-/]*$/u.test(tag) && !RESERVED.test(tag) ? tag : JSON.stringify(tag)
}

/**
 * What the reader typed, as a tag: `#` dropped, spaces joined up (a tag has none, and a space
 * is what someone types when they mean a hyphen), anything else the syntax cannot hold removed,
 * then normalised the way the index normalises — trimmed, unslashed, lower-cased. Empty when
 * nothing usable is left.
 */
export function cleanTag(input: string): string {
  const kept = [...input.trim().replace(/^#/u, '').replace(/\s+/gu, '-')].filter((c) => TAG_CHAR.test(c)).join('')
  return kept.replace(/^\/+/u, '').replace(/\/+$/u, '').toLowerCase()
}

/** A CodeMirror change: the span to replace and what to put there. */
export interface TagEdit {
  from: number
  to: number
  insert: string
}

/**
 * Where `tag` goes in `doc`. Returns the edit, or null when the note has no front matter this
 * can be added to *and* one cannot be made — which does not happen, since a note with no front
 * matter gets one.
 */
export function addTagToFrontMatter(doc: string, tag: string): TagEdit | null {
  if (!tag) return null
  const value = scalar(tag)
  const open = /^---[ \t]*\r?\n/u.exec(doc)
  const block = open && /^---[ \t]*\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n|$)/u.exec(doc)
  // No front matter to join: write one. The note's own `id:` normally means there already is.
  if (!open || !block) return { from: 0, to: 0, insert: `---\ntags: [${value}]\n---\n\n` }

  const start = open[0].length
  const body = block[1] ?? ''
  const lines = body.split('\n')
  // Offset of the first character of each line, in the document.
  const at: number[] = []
  let cursor = start
  for (const line of lines) {
    at.push(cursor)
    cursor += line.length + 1
  }

  const key = lines.findIndex((l) => /^tags[ \t]*:/u.test(l))
  // No `tags:` at all: a new line at the end of the front matter, just above its closing rule.
  if (key === -1) {
    const end = start + body.length
    return { from: end, to: end, insert: `\ntags: [${value}]` }
  }

  const line = lines[key] ?? ''
  const lineAt = at[key] ?? start
  const rest = line.slice(line.indexOf(':') + 1)
  const trimmed = rest.trim()

  // `tags: [a, b]` — join the flow list, which stays on one line.
  if (trimmed.startsWith('[')) {
    const close = line.lastIndexOf(']')
    if (close !== -1) {
      const inside = line.slice(line.indexOf('[') + 1, close).trim()
      return { from: lineAt + close, to: lineAt + close, insert: inside ? `, ${value}` : value }
    }
  }

  // `tags: a, b` — the comma-separated string form, which both indexers split.
  if (trimmed.length > 0) {
    const end = lineAt + line.replace(/[ \t]+$/u, '').length
    return { from: end, to: end, insert: `, ${value}` }
  }

  // `tags:` with the list underneath it. Follow the last item, at its own indent.
  const item = /^([ \t]+)-[ \t]*\S/u
  let last = -1
  for (let i = key + 1; i < lines.length; i++) {
    if (!item.test(lines[i] ?? '')) break
    last = i
  }
  if (last !== -1) {
    const indent = item.exec(lines[last] ?? '')?.[1] ?? '  '
    const end = (at[last] ?? start) + (lines[last] ?? '').length
    return { from: end, to: end, insert: `\n${indent}- ${value}` }
  }
  // `tags:` and nothing under it: the key is empty, so give it a list of its own.
  return { from: lineAt, to: lineAt + line.length, insert: `tags: [${value}]` }
}
