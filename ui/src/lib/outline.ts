import type { SyntaxNode } from '@lezer/common'

/** One heading in a note, as the editor found it. The margin index and the palette both
 *  order by `pos`, so it is the document offset rather than a line number. */
export interface OutlineItem {
  level: number
  text: string
  pos: number
}

/** Inline nodes a heading's outline entry leaves out: raw HTML shows nothing of itself once
 *  rendered (`## <span id="x">Setup</span>` reads "Setup"), and neither does a comment. */
const HIDDEN = new Set(['HTMLTag', 'Comment', 'HeaderMark'])

/** The text of an `ATXHeading` node as the outline lists it: its source without the `#`
 *  marker, inline HTML tags or comments, and with the gaps they leave collapsed. */
export function headingText(heading: SyntaxNode, slice: (from: number, to: number) => string): string {
  let out = ''
  let at = heading.from
  for (let c = heading.firstChild; c; c = c.nextSibling) {
    if (!HIDDEN.has(c.name)) continue
    out += slice(at, c.from)
    at = c.to
  }
  out += slice(at, heading.to)
  return out.replace(/\s+/gu, ' ').trim()
}
