// Markdown indexing for the SPEC §5 dialect — the TypeScript twin of
// `crates/core/src/markdown.rs`. Both must produce identical `NoteIndex` JSON for every case in
// `corpus/`; the shared test is `test/corpus.test.ts` here and `markdown::tests::corpus` there.
//
// The parser is micromark (mdast-util-from-markdown); markdown-rs on the Rust side is a port of
// the same state machine, which is what makes byte-for-byte agreement realistic.

import { fromMarkdown } from 'mdast-util-from-markdown'
import { gfm } from 'micromark-extension-gfm'
import { gfmFromMarkdown } from 'mdast-util-gfm'
import { math } from 'micromark-extension-math'
import { mathFromMarkdown } from 'mdast-util-math'
import { frontmatter } from 'micromark-extension-frontmatter'
import { frontmatterFromMarkdown } from 'mdast-util-frontmatter'
import type { Nodes, Root } from 'mdast'
import { parseFrontMatter, pushTag, type FrontMatter } from './frontmatter.ts'

export type { FrontMatter }
export { frontMatter, pushTag } from './frontmatter.ts'

export interface WikiLink {
  target: string
  heading?: string
  label?: string
  embed: boolean
}

export interface Heading {
  depth: number
  text: string
}

export interface NoteIndex {
  /** Front-matter `title`, else the first H1, else null (caller falls back to the filename). */
  title: string | null
  front_matter: FrontMatter | null
  /** Lower-cased, deduplicated, first-seen order; inline `#tags` first, then front matter. */
  tags: string[]
  wikilinks: WikiLink[]
  /** Destinations of `[text](url)` and `![](url)`, verbatim. */
  links: string[]
  headings: Heading[]
  has_math: boolean
  has_tasks: boolean
  /** Fenced code languages, braces stripped (`{python}` → `python`), deduplicated. */
  code_langs: string[]
  /** Markup-free text for full-text search; not part of the cross-parser contract. */
  plain_text: string
}

export function parseTree(source: string): Root {
  return fromMarkdown(source, {
    extensions: [gfm(), math(), frontmatter(['yaml'])],
    mdastExtensions: [gfmFromMarkdown(), mathFromMarkdown(), frontmatterFromMarkdown(['yaml'])],
  })
}

export function index(source: string): NoteIndex {
  const ix: NoteIndex = {
    title: null,
    front_matter: null,
    tags: [],
    wikilinks: [],
    links: [],
    headings: [],
    has_math: false,
    has_tasks: false,
    code_langs: [],
    plain_text: '',
  }
  const plain: string[] = []
  walk(parseTree(source), ix, plain)
  ix.plain_text = plain.join('\n').split(/\s+/u).filter(Boolean).join(' ')

  const fm = ix.front_matter
  if (fm) {
    if (fm.title !== null) ix.title = fm.title
    for (const t of fm.tags) pushTag(ix.tags, t)
  }
  if (ix.title === null) {
    const h1 = ix.headings.find((h) => h.depth === 1)
    ix.title = h1 ? h1.text : null
  }
  return ix
}

function walk(node: Nodes, ix: NoteIndex, plain: string[]): void {
  switch (node.type) {
    case 'yaml':
      ix.front_matter = parseFrontMatter(node.value)
      break
    case 'heading': {
      const text = inlineText(node.children)
      scanInline(text, ix)
      plain.push(text)
      collectLinks(node.children, ix)
      ix.headings.push({ depth: node.depth, text })
      break
    }
    case 'paragraph': {
      const text = inlineText(node.children)
      scanInline(text, ix)
      plain.push(text)
      collectLinks(node.children, ix)
      break
    }
    case 'math':
    case 'inlineMath':
      ix.has_math = true
      break
    case 'listItem':
      if (node.checked !== null && node.checked !== undefined) ix.has_tasks = true
      break
    case 'code': {
      if (node.lang) {
        const lang = node.lang.replace(/^[{}]+/u, '').replace(/[{}]+$/u, '')
        if (lang && !ix.code_langs.includes(lang)) ix.code_langs.push(lang)
      }
      break
    }
    default:
      break
  }
  if ('children' in node) for (const child of node.children) walk(child as Nodes, ix, plain)
}

function collectLinks(nodes: Nodes[], ix: NoteIndex): void {
  for (const n of nodes) {
    if (n.type === 'link' || n.type === 'image') ix.links.push(n.url)
    if ('children' in n) collectLinks(n.children as Nodes[], ix)
  }
}

/** Concatenated text of inline children, skipping code and math (no tags/links live there). */
function inlineText(nodes: Nodes[]): string {
  let s = ''
  for (const n of nodes) {
    switch (n.type) {
      case 'text':
        s += n.value
        break
      case 'inlineCode':
      case 'inlineMath':
        s += ' '
        break
      case 'break':
        s += '\n'
        break
      default:
        if ('children' in n) s += inlineText(n.children as Nodes[])
    }
  }
  return s
}

const ALNUM = /^[\p{Alphabetic}\p{N}]$/u
const TAG_CHAR = /^[\p{Alphabetic}\p{N}_\-/]$/u
const LETTER = /\p{Alphabetic}/u

function prevChar(text: string, i: number): string {
  if (i === 0) return ''
  const cp = text.codePointAt(i - 1)
  // If i-1 is a low surrogate, the character starts one unit earlier.
  const unit = text.charCodeAt(i - 1)
  if (unit >= 0xdc00 && unit <= 0xdfff && i >= 2) return String.fromCodePoint(text.codePointAt(i - 2)!)
  return String.fromCodePoint(cp!)
}

/** Find `#tags` and `[[wikilinks]]` in already-parsed inline text. */
function scanInline(text: string, ix: NoteIndex): void {
  let i = 0
  while (i < text.length) {
    if (text[i] === '[' && text[i + 1] === '[') {
      const embed = i > 0 && text[i - 1] === '!'
      const end = text.indexOf(']]', i + 2)
      if (end !== -1) {
        const inner = text.slice(i + 2, end)
        if (inner.length > 0 && !inner.includes('[[')) {
          ix.wikilinks.push(parseWikilink(inner, embed))
          i = end + 2
          continue
        }
      }
    }
    if (text[i] === '#') {
      const boundary = i === 0 || !ALNUM.test(prevChar(text, i))
      if (boundary) {
        let body = ''
        for (const ch of text.slice(i + 1)) {
          if (!TAG_CHAR.test(ch)) break
          body += ch
        }
        if (LETTER.test(body) && !body.startsWith('/')) {
          pushTag(ix.tags, body)
          i += 1 + body.length
          continue
        }
      }
    }
    i += 1
  }
}

function parseWikilink(inner: string, embed: boolean): WikiLink {
  const pipe = inner.indexOf('|')
  const targetPart = pipe === -1 ? inner : inner.slice(0, pipe)
  const labelRaw = pipe === -1 ? undefined : inner.slice(pipe + 1).trim()
  const hash = targetPart.indexOf('#')
  const target = (hash === -1 ? targetPart : targetPart.slice(0, hash)).trim()
  const headingRaw = hash === -1 ? undefined : targetPart.slice(hash + 1).trim()
  const link: WikiLink = { target, embed }
  if (headingRaw) link.heading = headingRaw
  if (labelRaw) link.label = labelRaw
  return link
}
