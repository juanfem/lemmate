// Note transclusion (SPEC §5, tier 3): what part of a note `![[note]]`, `![[note#Heading]]` or
// `![[note#^block]]` shows. Pure text in, text out — the widget that draws it lives in
// `livePreview.ts`, and the note it reads comes from the session.

export interface EmbedTarget {
  /** The note, as a wikilink names it. */
  note: string
  /** `#Heading` — the last one, when a path of them is given (`#Part#Chapter`). */
  heading?: string
  /** `#^id` — a paragraph or list item marked with that block id. */
  block?: string
}

/** Split `Note#Heading|alias` into what it names; the alias only ever labels a link. */
export function parseEmbed(inner: string): EmbedTarget {
  const [ref] = inner.split(/\\?\|/u, 1)
  const parts = ref!.split('#')
  const note = parts[0]!.trim()
  const rest = parts.slice(1).map((p) => p.trim()).filter(Boolean)
  const last = rest[rest.length - 1]
  if (last === undefined) return { note }
  if (last.startsWith('^')) return { note, block: last.slice(1) }
  return { note, heading: last }
}

/** A note's text without its front matter: the properties belong to the note, not the page. */
export function stripFrontMatter(text: string): string {
  if (!text.startsWith('---\n') && !text.startsWith('---\r\n')) return text
  const m = /\r?\n(?:---|\.\.\.)[ \t]*(?:\r?\n|$)/u.exec(text.slice(3))
  return m ? text.slice(3 + m.index + m[0].length) : text
}

const FENCE = /^\s{0,3}(`{3,}|~{3,})/u

/** Each line with whether it sits inside a fenced code block, where `#` is not a heading. */
function linesOutsideCode(lines: string[]): boolean[] {
  const out: boolean[] = []
  let fence: string | null = null
  for (const line of lines) {
    const m = FENCE.exec(line)
    if (fence === null) {
      out.push(true)
      if (m) fence = m[1]!
    } else {
      out.push(false)
      if (m && m[1]![0] === fence[0] && m[1]!.length >= fence.length && line.trim() === m[1]) fence = null
    }
  }
  return out
}

const HEADING = /^\s{0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/u

/** How a heading is matched: case, surrounding space and inner runs of space do not count. */
const norm = (s: string) => s.trim().replace(/\s+/gu, ' ').toLowerCase()

/**
 * The part of `text` an embed shows: the whole note, the section under a heading (the heading
 * included, up to the next heading of the same or a higher level), or the block carrying a
 * `^id`. `null` when the heading or block is not there — which the widget says, rather than
 * quietly showing the whole note.
 */
export function embeddedSection(text: string, target: EmbedTarget): string | null {
  const body = stripFrontMatter(text)
  if (target.heading !== undefined) return headingSection(body, target.heading)
  if (target.block !== undefined) return blockSection(body, target.block)
  return body.replace(/^\s*\n/u, '').replace(/\s+$/u, '')
}

function headingSection(body: string, heading: string): string | null {
  const lines = body.split('\n')
  const prose = linesOutsideCode(lines)
  const want = norm(heading)
  let start = -1
  let level = 0
  for (let i = 0; i < lines.length; i++) {
    if (!prose[i]) continue
    const m = HEADING.exec(lines[i]!)
    if (!m) continue
    if (start === -1) {
      if (norm(m[2] ?? '') === want) {
        start = i
        level = m[1]!.length
      }
    } else if (m[1]!.length <= level) {
      return lines.slice(start, i).join('\n').replace(/\s+$/u, '')
    }
  }
  return start === -1 ? null : lines.slice(start).join('\n').replace(/\s+$/u, '')
}

const LIST_ITEM = /^\s*(?:[-*+]|\d+[.)])\s/u

function blockSection(body: string, id: string): string | null {
  const lines = body.split('\n')
  const prose = linesOutsideCode(lines)
  const escaped = id.replace(/[.*+?^${}()|[\]\\]/gu, '\\$&')
  const marker = new RegExp(`(?:^|\\s)\\^${escaped}\\s*$`, 'u')
  const at = lines.findIndex((l, i) => prose[i] && marker.test(l))
  if (at === -1) return null
  const blank = (i: number) => lines[i]!.trim() === ''
  // A marker on a line of its own names the block above it (a table, a quote, a list).
  let end = at
  if (lines[at]!.trim() === `^${id}`) {
    end = at - 1
    while (end >= 0 && blank(end)) end--
    if (end < 0) return null
  }
  // A list item is its own block; anything else is the paragraph around the marker.
  let start = end
  if (!LIST_ITEM.test(lines[end]!)) while (start > 0 && !blank(start - 1)) start--
  return lines
    .slice(start, end + 1)
    .map((l) => l.replace(marker, ''))
    .join('\n')
}
