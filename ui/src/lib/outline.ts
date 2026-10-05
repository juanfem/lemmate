import type { SyntaxNode } from '@lezer/common'

/** One heading in a note, as the editor found it. The margin index and the palette both
 *  order by `pos`, so it is the document offset rather than a line number. */
export interface OutlineItem {
  level: number
  text: string
  pos: number
}

/** Syntax that shows nothing of itself once rendered: emphasis and code marks, link brackets,
 *  the `#` marker — and raw HTML (`## <span id="x">Setup</span>` reads "Setup") or a comment. */
const HIDDEN = new Set([
  'HeaderMark',
  'EmphasisMark',
  'StrikethroughMark',
  'CodeMark',
  'LinkMark',
  'HTMLTag',
  'Comment',
  'WikiEmbed',
])
/** A link's destination parts: hidden inside `[text](url "title")`, while a bare URL is text. */
const DESTINATION = new Set(['URL', 'LinkTitle', 'LinkLabel'])

/** The text of an `ATXHeading` node as the outline lists it — as it reads in the editor's
 *  live preview, not as it is written: no markup, a link by its text, a wikilink by its alias
 *  or target, an escape by its character, and with the gaps all that leaves collapsed. */
export function headingText(heading: SyntaxNode, slice: (from: number, to: number) => string): string {
  return plain(heading, slice).replace(/\s+/gu, ' ').trim()
}

function plain(node: SyntaxNode, slice: (from: number, to: number) => string): string {
  let out = ''
  let at = node.from
  for (let c = node.firstChild; c; c = c.nextSibling) {
    out += slice(at, c.from)
    at = c.to
    if (HIDDEN.has(c.name) || (DESTINATION.has(c.name) && /^(Link|Image)$/u.test(node.name))) continue
    if (c.name === 'WikiLink') {
      const [target, label] = slice(c.from + 2, c.to - 2).split('|', 2)
      out += (label ?? target!).trim()
    } else if (c.name === 'Escape') out += slice(c.from + 1, c.to)
    else out += plain(c, slice)
  }
  return out + slice(at, node.to)
}
