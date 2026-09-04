// Editing a note's tags as tags rather than as text. Adding one is a declaration about the
// note, so it goes in the front matter — the same place `tags:` already reads from, and the
// place it can be taken off again without hunting through the prose. Renaming and removing have
// to go further: a tag lives in the front matter *and* in the sentences, and both have to move.
//
// This is surgery on YAML somebody wrote by hand, so it meets the note where it is: whichever
// of the three list shapes it already uses, a tag joins it rather than converting it.

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

/** The front matter block's body and where it starts, or null when there is none. */
function frontMatterSpan(doc: string): { start: number; body: string } | null {
  const open = /^---[ \t]*\r?\n/u.exec(doc)
  const block = open && /^---[ \t]*\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n|$)/u.exec(doc)
  return open && block ? { start: open[0].length, body: block[1] ?? '' } : null
}

/** Where each line of `body` begins, in the document. */
function lineOffsets(body: string, start: number): { lines: string[]; at: number[] } {
  const lines = body.split('\n')
  const at: number[] = []
  let cursor = start
  for (const line of lines) {
    at.push(cursor)
    cursor += line.length + 1
  }
  return { lines, at }
}

/** A CodeMirror change: the span to replace and what to put there. */
export interface TagEdit {
  from: number
  to: number
  insert: string
}

/**
 * Where `tag` goes in `doc` — a targeted insert, so that the note's own spacing and list style
 * survive being added to. A note with no front matter gets one; the note's `id:` normally means
 * there already is one.
 */
export function addTagToFrontMatter(doc: string, tag: string): TagEdit | null {
  if (!tag) return null
  const value = scalar(tag)
  const fm = frontMatterSpan(doc)
  if (!fm) return { from: 0, to: 0, insert: `---\ntags: [${value}]\n---\n\n` }

  const { lines, at } = lineOffsets(fm.body, fm.start)
  const key = lines.findIndex((l) => /^tags[ \t]*:/u.test(l))
  // No `tags:` at all: a new line at the end of the front matter, just above its closing rule.
  if (key === -1) {
    const end = fm.start + fm.body.length
    return { from: end, to: end, insert: `\ntags: [${value}]` }
  }

  const line = lines[key] ?? ''
  const lineAt = at[key] ?? fm.start
  const trimmed = line.slice(line.indexOf(':') + 1).trim()

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
    const end = (at[last] ?? fm.start) + (lines[last] ?? '').length
    return { from: end, to: end, insert: `\n${indent}- ${value}` }
  }
  // `tags:` and nothing under it: the key is empty, so give it a list of its own.
  return { from: lineAt, to: lineAt + line.length, insert: `tags: [${value}]` }
}

// ---------------------------------------------------------------------------------------------
// Renaming and removing. Unlike adding, these have to find every mention of a tag — in the front
// matter and in the prose — so they rewrite the whole declaration rather than nudging it.

/** Where the `tags:` declaration is, what it holds, and how to write it back. */
interface TagList {
  from: number
  to: number
  /** The declared tags, unquoted and lower-cased, in the order the note writes them. */
  items: string[]
  /** The declaration for a new list — empty when the list is, so the key goes with it. */
  emit: (items: string[]) => string
}

/** Undo whatever quoting `scalar()` may have applied, and normalise for comparison. */
function unscalar(raw: string): string {
  const t = raw.trim()
  const quoted = /^"([^"]*)"$/u.exec(t) ?? /^'([^']*)'$/u.exec(t)
  return (quoted?.[1] ?? t).trim().toLowerCase()
}

function findTagList(doc: string): TagList | null {
  const fm = frontMatterSpan(doc)
  if (!fm) return null
  const { lines, at } = lineOffsets(fm.body, fm.start)
  const key = lines.findIndex((l) => /^tags[ \t]*:/u.test(l))
  if (key === -1) return null

  const line = lines[key] ?? ''
  const from = at[key] ?? fm.start
  const rest = line.slice(line.indexOf(':') + 1).trim()
  const oneLine = (join: (xs: string[]) => string) => (items: string[]) =>
    items.length ? `tags: ${join(items.map(scalar))}` : ''

  // `tags: [a, b]`
  if (rest.startsWith('[') && rest.endsWith(']')) {
    const inside = rest.slice(1, -1).trim()
    return {
      from,
      to: from + line.length,
      items: inside ? inside.split(',').map(unscalar).filter(Boolean) : [],
      emit: oneLine((xs) => `[${xs.join(', ')}]`),
    }
  }
  // `tags: a, b`
  if (rest.length > 0) {
    return {
      from,
      to: from + line.length,
      items: rest.split(',').map(unscalar).filter(Boolean),
      emit: oneLine((xs) => xs.join(', ')),
    }
  }
  // `tags:` with `- item` lines under it, at whatever indent they use.
  const item = /^([ \t]+)-[ \t]*(\S.*)$/u
  const items: string[] = []
  let last = key
  let indent = '  '
  for (let i = key + 1; i < lines.length; i++) {
    const m = item.exec(lines[i] ?? '')
    if (!m) break
    indent = m[1] ?? indent
    items.push(unscalar(m[2] ?? ''))
    last = i
  }
  return {
    from,
    to: (at[last] ?? from) + (lines[last] ?? '').length,
    items: items.filter(Boolean),
    emit: (xs) => (xs.length ? `tags:\n${xs.map((x) => `${indent}- ${scalar(x)}`).join('\n')}` : ''),
  }
}

/** Apply `next` to the declared tags. Null when the list would not change. */
function rewriteTagList(doc: string, next: (items: string[]) => string[]): string | null {
  const list = findTagList(doc)
  if (!list) return null
  const after = next(list.items)
  if (after.length === list.items.length && after.every((t, i) => t === list.items[i])) return null
  const emitted = list.emit(after)
  // An emptied list takes its own line with it rather than leaving a bare `tags:` behind.
  const to = emitted === '' && doc[list.to] === '\n' ? list.to + 1 : list.to
  return doc.slice(0, list.from) + emitted + doc.slice(to)
}

/**
 * Run `fn` over the parts of a note where an inline `#tag` can actually live. The front matter
 * is handled separately, and neither indexer sees a tag inside code — so a `#tag` in a fence or
 * a `code span` is prose *about* a tag, not one, and a mass edit must leave it where it is.
 */
function outsideCode(text: string, fn: (part: string) => string): string {
  const fm = frontMatterSpan(text)
  const headEnd = fm ? fm.start + fm.body.length + 4 : 0
  let fence: string | null = null
  const body = text
    .slice(headEnd)
    .split('\n')
    .map((line) => {
      const rule = /^[ \t]{0,3}(`{3,}|~{3,})/u.exec(line)?.[1]
      if (fence !== null) {
        if (rule && rule[0] === fence[0] && rule.length >= fence.length) fence = null
        return line
      }
      if (rule) {
        fence = rule
        return line
      }
      // Within a line, backtick runs are code and the pieces between them are not.
      return line
        .split(/(`+[^`]*`+)/u)
        .map((part, i) => (i % 2 === 0 ? fn(part) : part))
        .join('')
    })
    .join('\n')
  return text.slice(0, headEnd) + body
}

/** Inline `#tag` occurrences, matched the way `parseTag` matches them. */
const INLINE = /(?<![\p{Alphabetic}\p{N}])#([\p{Alphabetic}\p{N}_\-/]+)/gu
const isTagBody = (body: string) => /\p{Alphabetic}/u.test(body) && !body.startsWith('/')

/**
 * Rename a tag through a whole note: front matter and prose. Nested tags follow their parent —
 * renaming `#projects` to `#work` makes `#projects/alpha` into `#work/alpha`, because a branch
 * you cannot rename as a branch is a branch you have to rename by hand. Null when nothing moved.
 */
export function renameTagInText(text: string, from: string, to: string): string | null {
  if (!from || !to || from === to) return null
  const moved = (t: string) => (t === from ? to : t.startsWith(`${from}/`) ? to + t.slice(from.length) : null)
  const declared = rewriteTagList(text, (items) => {
    const next: string[] = []
    for (const t of items) {
      const x = moved(t) ?? t
      if (!next.includes(x)) next.push(x)
    }
    return next
  })
  const both = outsideCode(declared ?? text, (part) =>
    part.replace(INLINE, (whole, body: string) => {
      if (!isTagBody(body)) return whole
      const x = moved(body.toLowerCase())
      return x === null ? whole : `#${x}`
    }),
  )
  return both === text ? null : both
}

/**
 * Take a tag off a note: out of the front matter, and out of the prose, where the sentence it
 * was standing in has to close up behind it. Nested tags are left alone — `#projects/alpha` is
 * its own tag, and removing `#projects` says nothing about it. Null when the note lacked it.
 */
export function removeTagFromText(text: string, tag: string): string | null {
  if (!tag) return null
  const declared = rewriteTagList(text, (items) => items.filter((t) => t !== tag))
  const both = outsideCode(declared ?? text, (part) =>
    part
      // Cut to a mark rather than to nothing: which of the spaces around it goes with the tag
      // depends on where in the line the tag stood, and the mark is what keeps that answerable.
      // Markdown holds no NUL, so nothing already in the note can be mistaken for one.
      .replace(INLINE, (whole, body: string) => (isTagBody(body) && body.toLowerCase() === tag ? '\u0000' : whole))
      // The space in front, for a tag inside a sentence; the one behind, for a tag that opened
      // the line; neither, for a tag that was the whole of it.
      .replace(/[ \t]\u0000/gu, '')
      .replace(/\u0000[ \t]/gu, '')
      .replace(/\u0000/gu, ''),
  )
  return both === text ? null : both
}
