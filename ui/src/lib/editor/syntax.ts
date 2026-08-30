// Lezer markdown extensions for the SPEC §5 dialect pieces lang-markdown lacks: wikilinks,
// tags, and TeX math. They produce syntax nodes the live-preview plugin decorates.

import type { BlockContext, InlineContext, Line, MarkdownExtension } from '@lezer/markdown'
import { Tag, styleTags, tags as t } from '@lezer/highlight'

export const wikiLinkTag = Tag.define()
export const noteTagTag = Tag.define()
export const mathTag = Tag.define()

const TAG_CHAR = /[\p{Alphabetic}\p{N}_\-/]/u
const ALNUM = /[\p{Alphabetic}\p{N}]/u

function parseWikiLink(cx: InlineContext, next: number, pos: number): number {
  // "[["
  if (next !== 91 || cx.char(pos + 1) !== 91) return -1
  const close = cx.slice(pos + 2, cx.end).indexOf(']]')
  if (close <= 0) return -1
  const inner = cx.slice(pos + 2, pos + 2 + close)
  if (inner.includes('[[') || inner.includes('\n')) return -1
  const embed = pos > cx.offset && cx.char(pos - 1) === 33
  const from = embed ? pos - 1 : pos
  const to = pos + 2 + close + 2
  return cx.addElement(cx.elt(embed ? 'WikiEmbed' : 'WikiLink', from, to))
}

function parseTag(cx: InlineContext, next: number, pos: number): number {
  if (next !== 35) return -1 // '#'
  if (pos > cx.offset && ALNUM.test(String.fromCodePoint(cx.char(pos - 1)))) return -1
  let end = pos + 1
  while (end < cx.end && TAG_CHAR.test(String.fromCodePoint(cx.char(end)))) end++
  const body = cx.slice(pos + 1, end)
  if (!/\p{Alphabetic}/u.test(body) || body.startsWith('/')) return -1
  return cx.addElement(cx.elt('NoteTag', pos, end))
}

function parseInlineMath(cx: InlineContext, next: number, pos: number): number {
  if (next !== 36 || cx.char(pos + 1) === 36) return -1 // '$' but not '$$'
  const after = cx.char(pos + 1)
  if (after === 32 || after === -1) return -1
  for (let i = pos + 2; i < cx.end; i++) {
    const c = cx.char(i)
    if (c === 10) return -1
    if (c === 36 && cx.char(i - 1) !== 32 && cx.char(i - 1) !== 92) {
      return cx.addElement(cx.elt('InlineMath', pos, i + 1))
    }
  }
  return -1
}

const blockMath = {
  name: 'BlockMath',
  parse(cx: BlockContext, line: Line): boolean {
    if (!line.text.trimStart().startsWith('$$')) return false
    const start = cx.lineStart + line.pos
    const single = line.text.trim().length > 2 && line.text.trim().endsWith('$$')
    let end = cx.lineStart + line.text.length
    if (!single) {
      while (cx.nextLine()) {
        end = cx.lineStart + line.text.length
        if (line.text.trim().endsWith('$$')) break
      }
    }
    cx.addElement(cx.elt('BlockMath', start, end))
    cx.nextLine()
    return true
  },
}

export const noteSyntax: MarkdownExtension = [
  {
    defineNodes: [
      { name: 'WikiLink', style: wikiLinkTag },
      { name: 'WikiEmbed', style: wikiLinkTag },
      { name: 'NoteTag', style: noteTagTag },
      { name: 'InlineMath', style: mathTag },
      { name: 'BlockMath', style: mathTag, block: true },
    ],
    parseInline: [
      { name: 'WikiLink', parse: parseWikiLink, before: 'Link' },
      { name: 'NoteTag', parse: parseTag },
      { name: 'InlineMath', parse: parseInlineMath, before: 'Escape' },
    ],
    parseBlock: [{ ...blockMath, before: 'FencedCode' }],
    props: [styleTags({ 'WikiLink WikiEmbed': wikiLinkTag, NoteTag: noteTagTag, 'InlineMath BlockMath': mathTag })],
  },
]

export { t as highlightTags }
