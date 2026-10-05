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

export function parseTree(source: string, htmlBlocks = true): Root {
  const extensions = [gfm(), math(), frontmatter(['yaml'])]
  if (!htmlBlocks) extensions.push({ disable: { null: ['htmlFlow'] } })
  return fromMarkdown(source, {
    extensions,
    mdastExtensions: [gfmFromMarkdown(), mathFromMarkdown(), frontmatterFromMarkdown(['yaml'])],
  })
}

/** Container markers (`>`, list bullets and numbers) a line may open before the rest is text. */
export const MAX_CONTAINERS = 32
/** Columns of whitespace (a tab counts 4) a line's container prefix may hold. */
export const MAX_INDENT = 128
/** Emphasis delimiters (`*`, `~`, and `_` not inside a word) a block may hold. */
export const MAX_EMPHASIS = 500
/**
 * How deep `[` may nest within a block — and how many `]` closing nothing it may hold, each of
 * which sends the parser looking back through the whole block for a `[`.
 */
export const MAX_BRACKETS = 32

const ASCII_ALNUM = /^[A-Za-z0-9]$/u
const ASCII_PUNCT = /^[!-/:-@[-`{-~]$/u

/**
 * The source with pathological nesting defused, before the parser sees it — the twin of
 * `markdown::tame`. Each level of nesting is a level of recursion and the parser's work grows
 * with the square of the depth, so a note of ten thousand `>` or `*` would overflow the stack or
 * hang the tab. No real note comes near these limits; past them, markers are escaped with `\`
 * and read as text. A *block* here is a run of non-blank lines.
 */
export function tame(source: string): string {
  const out: string[] = []
  let changed = false
  let emphasis = 0
  let brackets = 0
  let stray = 0
  const counted = (n: number, limit: number): [number, boolean] => (n >= limit ? [n, true] : [n + 1, false])
  for (const line of source.match(/[^\n]*\n|[^\n]+$/gu) ?? []) {
    if (/^[ \t\r\n]*$/u.test(line)) {
      emphasis = 0
      brackets = 0
      stray = 0
      out.push(line)
      continue
    }
    let i = 0
    let ws = 0
    let markers = 0
    for (;;) {
      const start = i
      let cols = 0
      while (line[i] === ' ' || line[i] === '\t') {
        cols += line[i] === '\t' ? 4 : 1
        i++
      }
      if (ws + cols > MAX_INDENT) {
        // At least one space stays, or the marker before it would stop being one.
        const keep = Math.max(MAX_INDENT - ws, 1)
        out.push(' '.repeat(keep))
        ws += keep
        changed = true
      } else {
        out.push(line.slice(start, i))
        ws += cols
      }
      const m = containerMarker(line, i)
      if (m === 0) break
      out.push(line.slice(i, i + m - 1))
      if (markers === MAX_CONTAINERS) {
        out.push('\\')
        changed = true
      }
      out.push(line[i + m - 1]!)
      i += m
      if (markers === MAX_CONTAINERS) break
      markers++
    }
    for (; i < line.length; i++) {
      const c = line[i]!
      if (c === '\\' && i + 1 < line.length && ASCII_PUNCT.test(line[i + 1]!)) {
        out.push(line.slice(i, i + 2))
        i++
        continue
      }
      let escape = false
      if (c === '*' || c === '~') [emphasis, escape] = counted(emphasis, MAX_EMPHASIS)
      else if (c === '_' && !(i > 0 && ASCII_ALNUM.test(line[i - 1]!) && ASCII_ALNUM.test(line[i + 1] ?? '')))
        [emphasis, escape] = counted(emphasis, MAX_EMPHASIS)
      else if (c === '[') [brackets, escape] = counted(brackets, MAX_BRACKETS)
      else if (c === ']' && brackets === 0) [stray, escape] = counted(stray, MAX_BRACKETS)
      else if (c === ']') brackets--
      if (escape) {
        out.push('\\')
        changed = true
      }
      out.push(c)
    }
  }
  return changed ? out.join('') : source
}

/** The length of the container marker at `i` — see `container_marker` in the Rust — or 0. */
function containerMarker(line: string, i: number): number {
  const spaced = (at: number) => at >= line.length || ' \t\r\n'.includes(line[at]!)
  const c = line[i]
  if (c === '>') return 1
  if ((c === '-' || c === '+' || c === '*') && spaced(i + 1)) return 1
  let n = 0
  while (i + n < line.length && line[i + n]! >= '0' && line[i + n]! <= '9') n++
  if (n > 0 && n <= 9 && (line[i + n] === '.' || line[i + n] === ')') && spaced(i + n + 1)) return n + 1
  return 0
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
  walk(parseTree(tame(source)), ix, plain)
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

/** Pre-order over the tree, with an explicit stack: however deep a note nests, no recursion. */
function walk(root: Nodes, ix: NoteIndex, plain: string[]): void {
  // Each node, and whether its parent holds blocks (so a raw HTML child is read for links).
  const stack: [Nodes, boolean][] = [[root, false]]
  for (let top = stack.pop(); top; top = stack.pop()) {
    const [node, inBlocks] = top
    if (inBlocks && node.type === 'html') htmlBlockLinks(node.value, ix)
    switch (node.type) {
      case 'yaml':
        ix.front_matter = parseFrontMatter(node.value)
        break
      case 'heading': {
        const [text, scan] = inlineText(node.children)
        scanInline(scan, ix)
        plain.push(text)
        collectLinks(node.children, ix)
        ix.headings.push({ depth: node.depth, text })
        break
      }
      // A table cell holds inline content just as a paragraph does, and the same links and tags.
      case 'paragraph':
      case 'tableCell': {
        const [text, scan] = inlineText(node.children)
        scanInline(scan, ix)
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
    if ('children' in node) {
      const holdsBlocks = ['root', 'blockquote', 'listItem', 'footnoteDefinition'].includes(node.type)
      for (let k = node.children.length - 1; k >= 0; k--) stack.push([node.children[k] as Nodes, holdsBlocks])
    }
  }
}

/**
 * The links in a raw HTML block. CommonMark runs a block that opens with a tag like `<p>` or
 * `<div>` on to the next blank line, so a Quarto slide's `<p class="cite">…</p>` followed by
 * `:::` and `![](figures/plot.png)` is all HTML, image included. Pandoc — what Quarto renders
 * with — reads the HTML as HTML and the lines after it as markdown, and shows the image. Read
 * again without HTML blocks, the tags are inline HTML and the image is an image.
 */
function htmlBlockLinks(html: string, ix: NoteIndex): void {
  collectLinks(parseTree(html, false).children as Nodes[], ix)
}

const ASCII_SPACE = /^[ \t\n\r\f]$/u

/**
 * The `src` attribute values in a run of HTML — `<img src="pic.png">` names a file just as
 * `![](pic.png)` does. Quoted or bare; the name is matched without regard to ASCII case.
 */
export function srcAttributes(html: string): string[] {
  const lower = html.replace(/[A-Z]/gu, (c) => c.toLowerCase())
  const space = (i: number) => i < lower.length && ASCII_SPACE.test(lower[i]!)
  const out: string[] = []
  let from = 0
  for (let at = lower.indexOf('src', from); at !== -1; at = lower.indexOf('src', from)) {
    from = at + 3
    if (at === 0 || !space(at - 1)) continue
    let i = at + 3
    while (space(i)) i++
    if (lower[i] !== '=') continue
    i++
    while (space(i)) i++
    if (i >= lower.length) continue
    let start: number
    let end: number
    const q = lower[i]!
    if (q === '"' || q === "'") {
      const close = lower.indexOf(q, i + 1)
      if (close === -1) continue
      ;[start, end] = [i + 1, close]
    } else {
      end = i
      while (end < lower.length && !space(end) && lower[end] !== '>') end++
      start = i
    }
    if (end > start) out.push(html.slice(start, end))
    from = end
  }
  return out
}

function collectLinks(nodes: Nodes[], ix: NoteIndex): void {
  const stack = [...nodes].reverse()
  for (let n = stack.pop(); n; n = stack.pop()) {
    if (n.type === 'link' || n.type === 'image') ix.links.push(n.url)
    if (n.type === 'html') ix.links.push(...srcAttributes(n.value))
    if ('children' in n) for (let k = n.children.length - 1; k >= 0; k--) stack.push(n.children[k] as Nodes)
  }
}

/**
 * Concatenated text of inline children, skipping code and math (no tags/links live there): the
 * text itself, and the same with every autolinked URL blanked out — what is scanned for tags and
 * wikilinks, so that `https://example.com/#anchor` does not tag the note `anchor`.
 */
function inlineText(nodes: Nodes[]): [string, string] {
  let s = ''
  let scan = ''
  const stack: [Nodes, boolean][] = [...nodes].reverse().map((n) => [n, false])
  for (let top = stack.pop(); top; top = stack.pop()) {
    const [n, url] = top
    switch (n.type) {
      case 'text':
        s += n.value
        scan += url ? ' ' : n.value
        break
      case 'inlineCode':
      case 'inlineMath':
        s += ' '
        scan += ' '
        break
      case 'break':
        s += '\n'
        scan += '\n'
        break
      default:
        if ('children' in n) {
          const inUrl = url || (n.type === 'link' && isAutolink(n.url, n.children as Nodes[]))
          for (let k = n.children.length - 1; k >= 0; k--) stack.push([n.children[k] as Nodes, inUrl])
        }
    }
  }
  return [s, scan]
}

/**
 * Whether a link is its own URL written out — `<https://…>`, `<a@b.c>`, or a bare URL or `www.`
 * address GFM links by itself — rather than text someone wrote for it.
 */
function isAutolink(url: string, children: Nodes[]): boolean {
  const only = children.length === 1 ? children[0] : undefined
  if (only?.type !== 'text') return false
  const text = only.value
  return url === text || url === `http://${text}` || url === `mailto:${text}`
}

const ALNUM = /^[\p{Alphabetic}\p{N}]$/u
const TAG_CHAR = /^[\p{Alphabetic}\p{N}_\-/]$/u
const LETTER = /\p{Alphabetic}/u
/** An ASCII character that cannot be in a tag; where the word a tag is read from ends. */
const WORD_END = /^[\x00-\x2c.:-@[-^`{-\x7f]$/u

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
    // The body is read composed (NFC), so `#áb` typed as `a` + a combining accent is `áb`.
    if (text[i] === '#') {
      const boundary = i === 0 || !ALNUM.test(prevChar(text, i))
      if (boundary) {
        let end = i + 1
        while (end < text.length && !WORD_END.test(text[end]!)) end++
        let body = ''
        for (const ch of text.slice(i + 1, end).normalize('NFC')) {
          if (!TAG_CHAR.test(ch)) break
          body += ch
        }
        if (LETTER.test(body) && !body.startsWith('/')) pushTag(ix.tags, body)
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
