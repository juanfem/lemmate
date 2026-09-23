// Live preview (SPEC §8): the source is always the document; markup is hidden and rendered in
// place, and revealed again on any line the selection touches. Lossless by construction.

import { Decoration, EditorView, WidgetType, type DecorationSet } from '@codemirror/view'
import { StateEffect, StateField, type EditorState } from '@codemirror/state'
import { syntaxTree } from '@codemirror/language'
import type { SyntaxNode } from '@lezer/common'
import katex from 'katex'
import { codeLanguageName } from './syntax.ts'
import { blockMarker, parseEmbed, type EmbedTarget } from './transclude.ts'

export interface LivePreviewOptions {
  /** Never reveal markup (read-only views have no meaningful cursor). */
  alwaysFolded?: boolean
  /** Called when a wikilink widget is activated. */
  openLink: (target: string) => void
  /** Resolve an embed target to a URL (attachments) or undefined. */
  embedUrl: (target: string) => string | undefined
  /**
   * The note an `![[embed]]` names, to draw in place of it (SPEC §5, tier 3). Undefined — or
   * left out, as views with no vault behind them do — keeps the embed a link.
   */
  embedNote?: (target: EmbedTarget) => EmbeddedNote | undefined
}

/** A transcluded note, as the widget that frames it needs it. */
export interface EmbeddedNote {
  /** What is shown — note and section. The widget, and what it follows, live while it holds. */
  key: string
  /** The frame's caption: the note's name, and the section when there is one. */
  title: string
  /** Draw the note into `host` and keep it current; the function returned stops and cleans up. */
  mount: (host: HTMLElement) => () => void
}

class MathWidget extends WidgetType {
  readonly tex: string
  readonly display: boolean
  constructor(tex: string, display: boolean) {
    super()
    this.tex = tex
    this.display = display
  }
  eq(other: MathWidget) {
    return other.tex === this.tex && other.display === this.display
  }
  toDOM() {
    const el = document.createElement(this.display ? 'div' : 'span')
    el.className = this.display ? 'cm-math-block' : 'cm-math-inline'
    try {
      katex.render(this.tex, el, { displayMode: this.display, throwOnError: false })
    } catch {
      el.textContent = this.tex
    }
    return el
  }
  ignoreEvent() {
    return false
  }
}

class LinkWidget extends WidgetType {
  readonly label: string
  readonly target: string
  readonly open: (t: string) => void
  constructor(label: string, target: string, open: (t: string) => void) {
    super()
    this.label = label
    this.target = target
    this.open = open
  }
  eq(other: LinkWidget) {
    return other.label === this.label && other.target === this.target
  }
  toDOM() {
    const a = document.createElement('a')
    a.className = 'cm-wikilink'
    a.textContent = this.label
    a.href = '#'
    a.onclick = (e) => {
      e.preventDefault()
      this.open(this.target)
    }
    return a
  }
  ignoreEvent(e: Event) {
    return e.type === 'click'
  }
}

class ImageWidget extends WidgetType {
  readonly url: string
  readonly alt: string
  constructor(url: string, alt: string) {
    super()
    this.url = url
    this.alt = alt
  }
  eq(other: ImageWidget) {
    return other.url === this.url
  }
  toDOM() {
    const img = document.createElement('img')
    img.className = 'cm-embed-image'
    img.src = this.url
    img.alt = this.alt
    return img
  }
}

/**
 * Draw the preview again with the document unchanged: what an embed or a link resolves to has
 * moved under it — the vault's note list arrived after the note did, or a note was renamed.
 */
export const refreshPreview = StateEffect.define<null>()

/** What each drawn embed must undo when CodeMirror drops its DOM. */
const unmounts = new WeakMap<HTMLElement, () => void>()

/**
 * `![[note]]` on a line of its own: the note, read-only and live, in a frame captioned with its
 * name. The caption opens it; a press anywhere else on the frame puts the caret on the embed,
 * which reveals the source — the same bargain a rendered table makes.
 */
class TranscludeWidget extends WidgetType {
  readonly embed: EmbeddedNote
  readonly note: string
  readonly open: (t: string) => void
  constructor(embed: EmbeddedNote, note: string, open: (t: string) => void) {
    super()
    this.embed = embed
    this.note = note
    this.open = open
  }
  eq(other: TranscludeWidget) {
    return other.embed.key === this.embed.key && other.embed.title === this.embed.title
  }
  get estimatedHeight() {
    return 120
  }
  toDOM(view: EditorView) {
    // The wrapper carries the gap around the frame as padding, for the reason `.cm-table` does.
    const wrap = document.createElement('div')
    wrap.className = 'cm-transclusion'
    const box = document.createElement('div')
    box.className = 'cm-transclusion-box'
    const caption = document.createElement('div')
    caption.className = 'cm-transclusion-caption'
    const title = document.createElement('a')
    title.className = 'cm-wikilink'
    title.href = '#'
    title.textContent = this.embed.title
    title.title = 'Open the note'
    title.onclick = (e) => {
      e.preventDefault()
      this.open(this.note)
    }
    caption.append(title)
    const body = document.createElement('div')
    body.className = 'cm-transclusion-body'
    box.append(caption, body)
    wrap.append(box)
    unmounts.set(wrap, this.embed.mount(body))
    wrap.addEventListener('mousedown', (e) => {
      const target = e.target as HTMLElement
      if (target.closest('a, .cm-transclusion-body .cm-content') || !view.state.facet(EditorView.editable)) return
      e.preventDefault()
      view.dispatch({ selection: { anchor: view.posAtDOM(wrap) } })
      view.focus()
    })
    return wrap
  }
  destroy(dom: HTMLElement) {
    unmounts.get(dom)?.()
    unmounts.delete(dom)
  }
  ignoreEvent() {
    return true
  }
}

class CheckboxWidget extends WidgetType {
  readonly checked: boolean
  constructor(checked: boolean) {
    super()
    this.checked = checked
  }
  eq(other: CheckboxWidget) {
    return other.checked === this.checked
  }
  toDOM() {
    const box = document.createElement('input')
    box.type = 'checkbox'
    box.checked = this.checked
    box.className = 'cm-task-checkbox'
    return box
  }
  ignoreEvent() {
    return false
  }
}

/**
 * The bullet a `-`/`*`/`+` marker renders as, by nesting depth (1 = outermost). Editors give
 * each level its own shape so the structure reads at a glance; the sequence is the CSS one,
 * disc → circle → square, and cycles past three. A task item renders nothing: its checkbox
 * is already the marker, and a bullet beside it is one marker too many. The empty widget still
 * takes the marker's width, so a task lines up with its bullet siblings.
 */
export function listBullet(depth: number, task: boolean): string {
  if (task) return ''
  const shapes = ['\u2022', '\u25e6', '\u25aa']
  return shapes[(Math.max(1, depth) - 1) % shapes.length]!
}

const ALPHA = 'abcdefghijklmnopqrstuvwxyz'
const ROMAN: [number, string][] = [
  [1000, 'm'], [900, 'cm'], [500, 'd'], [400, 'cd'], [100, 'c'], [90, 'xc'],
  [50, 'l'], [40, 'xl'], [10, 'x'], [9, 'ix'], [5, 'v'], [4, 'iv'], [1, 'i'],
]

/** `1 → a … 26 → z, 27 → aa`: the spreadsheet numbering CSS calls lower-alpha. */
function alpha(n: number): string {
  let out = ''
  for (let k = n; k > 0; k = Math.floor((k - 1) / 26)) out = ALPHA[(k - 1) % 26]! + out
  return out
}

function roman(n: number): string {
  let out = ''
  let left = n
  for (const [value, sign] of ROMAN) {
    while (left >= value) {
      out += sign
      left -= value
    }
  }
  return out
}

/**
 * How an ordered item's number is written, by nesting depth (1 = outermost) — the convention a
 * document uses: decimal, then lower-alpha, then lower-roman, cycling past three. `null` for a
 * number the numerals cannot spell, which keeps the digits the file holds.
 */
export function listNumber(depth: number, n: number): string | null {
  if (n < 1) return null
  const style = (Math.max(1, depth) - 1) % 3
  if (style === 0) return String(n)
  if (style === 1) return alpha(n)
  return n > 3999 ? null : roman(n)
}

class MarkerWidget extends WidgetType {
  readonly text: string
  readonly cls: string
  constructor(text: string, cls: string) {
    super()
    this.text = text
    this.cls = cls
  }
  eq(other: MarkerWidget) {
    return other.text === this.text && other.cls === this.cls
  }
  toDOM() {
    const el = document.createElement('span')
    el.className = this.cls
    el.textContent = this.text
    return el
  }
}

const hide = Decoration.replace({})

/** A cell's content, as the few inline shapes a rendered table draws. */
export type Inline =
  | { kind: 'text'; text: string }
  | { kind: 'em' | 'strong' | 's'; children: Inline[] }
  | { kind: 'code'; text: string }
  | { kind: 'link'; href: string; children: Inline[] }
  | { kind: 'wikilink'; target: string; label: string }
  | { kind: 'tag'; text: string }
  | { kind: 'math'; tex: string }

export interface TableCell {
  /** Offset of the cell's source from `base` (the widget's start) — where a click puts the caret. */
  at: number
  content: Inline[]
}

export interface TableModel {
  align: ('left' | 'center' | 'right' | null)[]
  header: (TableCell | null)[]
  rows: (TableCell | null)[][]
}

const WRAPPERS: Record<string, 'em' | 'strong' | 's'> = { Emphasis: 'em', StrongEmphasis: 'strong', Strikethrough: 's' }
const MARKS = new Set(['EmphasisMark', 'StrikethroughMark', 'CodeMark', 'LinkMark', 'URL', 'LinkTitle', 'LinkLabel'])

/** The inline content between `from` and `to` under `node`: children rendered, gaps as text. */
function inlines(doc: (f: number, t: number) => string, node: SyntaxNode, from: number, to: number): Inline[] {
  const out: Inline[] = []
  const text = (f: number, t: number) => {
    if (t > f) out.push({ kind: 'text', text: doc(f, t) })
  }
  let pos = from
  for (let c = node.firstChild; c; c = c.nextSibling) {
    if (c.from < from || c.to > to) continue
    text(pos, c.from)
    pos = c.to
    const src = doc(c.from, c.to)
    if (c.name in WRAPPERS) out.push({ kind: WRAPPERS[c.name]!, children: inlines(doc, c, c.from, c.to) })
    else if (c.name === 'InlineCode') out.push({ kind: 'code', text: src.replace(/^`+/u, '').replace(/`+$/u, '').trim() })
    else if (c.name === 'Link') {
      // `[text](url "title")`: the text is what sits between the first two marks.
      const url = c.getChild('URL')
      const marks = c.getChildren('LinkMark')
      const inner = marks.length >= 2 ? inlines(doc, c, marks[0]!.to, marks[1]!.from) : [{ kind: 'text' as const, text: src }]
      out.push({ kind: 'link', href: url ? doc(url.from, url.to) : '', children: inner })
    } else if (c.name === 'WikiLink' || c.name === 'WikiEmbed') {
      // Inside a table the alias pipe is written `\|`, or it would end the cell.
      const [targetPart, label] = src.slice(c.name === 'WikiLink' ? 2 : 3, -2).split(/\\?\|/u, 2)
      out.push({ kind: 'wikilink', target: targetPart!.split('#')[0]!.trim(), label: (label ?? targetPart!).trim() })
    } else if (c.name === 'NoteTag') out.push({ kind: 'tag', text: src })
    else if (c.name === 'InlineMath') out.push({ kind: 'math', tex: src.slice(1, -1) })
    else if (c.name === 'Escape') out.push({ kind: 'text', text: src.slice(1) })
    else if (!MARKS.has(c.name)) out.push({ kind: 'text', text: src })
  }
  text(pos, to)
  // Adjacent text runs (an escape splits one) read better, and compare equal, as one.
  return out.reduce<Inline[]>((acc, it) => {
    const last = acc[acc.length - 1]
    if (it.kind === 'text' && last?.kind === 'text') last.text += it.text
    else acc.push(it)
    return acc
  }, [])
}

/**
 * What a GFM `Table` node draws: the column alignment from the delimiter row, and each row's
 * cells by column. The parser leaves an empty cell out altogether, so a cell's column is the
 * count of pipes before it — less the leading one, when the row has it.
 */
export function tableModel(doc: (f: number, t: number) => string, table: SyntaxNode, base = table.from): TableModel {
  const model: TableModel = { align: [], header: [], rows: [] }
  const cells = (row: SyntaxNode): (TableCell | null)[] => {
    const out: (TableCell | null)[] = []
    let pipes = 0
    let leading = false
    for (let c = row.firstChild; c; c = c.nextSibling) {
      if (c.name === 'TableDelimiter') {
        if (c.from === row.from) leading = true
        pipes++
      } else if (c.name === 'TableCell') {
        const col = pipes - (leading ? 1 : 0)
        while (out.length < col) out.push(null)
        out[col] = { at: c.from - base, content: inlines(doc, c, c.from, c.to) }
      }
    }
    return out
  }
  for (let c = table.firstChild; c; c = c.nextSibling) {
    if (c.name === 'TableHeader') model.header = cells(c)
    else if (c.name === 'TableRow') model.rows.push(cells(c))
    else if (c.name === 'TableDelimiter') {
      model.align = doc(c.from, c.to)
        .replace(/^\s*\|/u, '')
        .replace(/\|\s*$/u, '')
        .split('|')
        .map((d) => {
          const s = d.trim()
          const l = s.startsWith(':')
          const r = s.endsWith(':')
          return l && r ? 'center' : r ? 'right' : l ? 'left' : null
        })
    }
  }
  // GFM gives the table the header's width: a short row is padded, a long one cut.
  const fit = (cells: (TableCell | null)[]) => Array.from({ length: model.align.length }, (_, i) => cells[i] ?? null)
  return { align: model.align, header: fit(model.header), rows: model.rows.map(fit) }
}

function renderInlines(parent: HTMLElement, content: Inline[], open: (t: string) => void) {
  for (const it of content) {
    switch (it.kind) {
      case 'text':
        parent.append(it.text)
        break
      case 'em':
      case 'strong':
      case 's': {
        const el = document.createElement(it.kind)
        renderInlines(el, it.children, open)
        parent.append(el)
        break
      }
      case 'code': {
        const el = document.createElement('code')
        el.textContent = it.text
        parent.append(el)
        break
      }
      case 'link': {
        const a = document.createElement('a')
        a.href = it.href
        a.target = '_blank'
        a.rel = 'noopener noreferrer'
        renderInlines(a, it.children, open)
        parent.append(a)
        break
      }
      case 'wikilink':
        parent.append(new LinkWidget(it.label, it.target, open).toDOM())
        break
      case 'tag': {
        const el = document.createElement('span')
        el.className = 'cm-tag'
        el.textContent = it.text
        parent.append(el)
        break
      }
      case 'math':
        parent.append(new MathWidget(it.tex, false).toDOM())
        break
    }
  }
}

class TableWidget extends WidgetType {
  readonly source: string
  readonly model: TableModel
  readonly open: (t: string) => void
  constructor(source: string, model: TableModel, open: (t: string) => void) {
    super()
    this.source = source
    this.model = model
    this.open = open
  }
  eq(other: TableWidget) {
    return other.source === this.source
  }
  toDOM(view: EditorView) {
    // The wrapper scrolls a table wider than the measure, and carries the gap around it as
    // padding — a margin would sit outside the rect the height map measures (see `.cm-heading`).
    const wrap = document.createElement('div')
    wrap.className = 'cm-table'
    const table = document.createElement('table')
    const row = (cells: (TableCell | null)[], tag: 'th' | 'td') => {
      const tr = document.createElement('tr')
      for (let i = 0; i < this.model.align.length; i++) {
        const el = document.createElement(tag)
        const align = this.model.align[i]
        if (align) el.style.textAlign = align
        const cell = cells[i]
        if (cell) {
          renderInlines(el, cell.content, this.open)
          el.dataset.at = String(cell.at)
        }
        tr.append(el)
      }
      return tr
    }
    const thead = document.createElement('thead')
    thead.append(row(this.model.header, 'th'))
    const tbody = document.createElement('tbody')
    for (const r of this.model.rows) tbody.append(row(r, 'td'))
    table.append(thead)
    if (this.model.rows.length > 0) table.append(tbody)
    wrap.append(table)
    // Clicking a cell edits it: the caret goes to the start of that cell's source, which puts
    // the selection on the table and reveals the markdown. Links inside still follow.
    wrap.addEventListener('mousedown', (e) => {
      const target = e.target as HTMLElement
      if (target.closest('a') || !view.state.facet(EditorView.editable)) return
      const at = target.closest<HTMLElement>('[data-at]')?.dataset.at
      const start = view.posAtDOM(wrap)
      e.preventDefault()
      view.dispatch({ selection: { anchor: start + (at === undefined ? 0 : Number(at)) } })
      view.focus()
    })
    return wrap
  }
  ignoreEvent() {
    return true
  }
}

class CalloutTitle extends WidgetType {
  readonly title: string
  constructor(title: string) {
    super()
    this.title = title
  }
  eq(other: CalloutTitle) {
    return other.title === this.title
  }
  toDOM() {
    const el = document.createElement('span')
    el.className = 'cm-callout-title'
    el.textContent = this.title.charAt(0).toUpperCase() + this.title.slice(1)
    return el
  }
}

/** Collapsed front matter: a one-line summary of the properties (SPEC §8). */
class FrontMatterWidget extends WidgetType {
  readonly summary: string
  constructor(summary: string) {
    super()
    this.summary = summary
  }
  eq(other: FrontMatterWidget) {
    return other.summary === this.summary
  }
  toDOM() {
    // An unstyled wrapper carries the gap below the box as padding: a margin on the box itself
    // would sit outside the rect CodeMirror measures this widget with, and shift every line
    // under it away from where the height map thinks it is.
    const wrap = document.createElement('div')
    wrap.className = 'cm-frontmatter'
    const box = document.createElement('div')
    box.className = 'cm-frontmatter-box'
    const label = document.createElement('span')
    label.textContent = this.summary || 'properties'
    const caret = document.createElement('span')
    caret.className = 'cm-frontmatter-caret'
    caret.textContent = '⌄'
    box.append(label, caret)
    box.title = 'Front matter — click to edit'
    wrap.appendChild(box)
    return wrap
  }
  ignoreEvent() {
    return false
  }
}

function frontMatterRange(state: EditorState): { from: number; to: number; body: string } | null {
  const first = state.doc.line(1)
  if (first.text.trim() !== '---') return null
  for (let n = 2; n <= Math.min(state.doc.lines, 200); n++) {
    const line = state.doc.line(n)
    if (line.text.trim() === '---' || line.text.trim() === '...') {
      return { from: first.from, to: line.to, body: state.sliceDoc(first.to + 1, line.from) }
    }
  }
  return null
}

/**
 * What the collapsed chip says: the property names, then how many there are — "id · 1
 * property". Names rather than values, because the chip has to stay chip-sized; the values
 * are one click away, and a long list truncates to the first few.
 */
function frontMatterSummary(body: string): string {
  const keys: string[] = []
  for (const line of body.split('\n')) {
    const m = /^([A-Za-z_][\w-]*):/u.exec(line)
    if (m) keys.push(m[1]!)
  }
  if (keys.length === 0) return ''
  const shown = keys.length > 3 ? [...keys.slice(0, 3), '…'] : keys
  return `${shown.join(' · ')} · ${keys.length} ${keys.length === 1 ? 'property' : 'properties'}`
}

/** Does any selection range touch the lines spanned by [from, to]? */
function revealedBySelection(state: EditorState, from: number, to: number): boolean {
  const a = state.doc.lineAt(from).from
  const b = state.doc.lineAt(to).to
  return state.selection.ranges.some((r) => r.from <= b && r.to >= a)
}

function build(state: EditorState, opts: LivePreviewOptions): DecorationSet {
  const revealed = opts.alwaysFolded ? () => false : revealedBySelection
  const items: { from: number; to: number; deco: Decoration }[] = []
  const push = (from: number, to: number, deco: Decoration) => items.push({ from, to, deco })

  // Pandoc fenced divs / Quarto callouts (SPEC §5.3): `::: {.callout-note title="…"}` … `:::`
  let inCallout = false
  for (let ln = 1; ln <= state.doc.lines; ln++) {
    const line = state.doc.line(ln)
    const t = line.text.trim()
    if (!inCallout && /^:{3,}\s*\{?\.?callout/u.test(t)) {
      inCallout = true
      push(line.from, line.from, Decoration.line({ class: 'cm-callout cm-callout-fence' }))
      const title = /title="([^"]*)"/u.exec(t)?.[1] ?? /callout-([a-z]+)/u.exec(t)?.[1] ?? 'note'
      if (!revealed(state, line.from, line.to)) push(line.from, line.to, Decoration.replace({ widget: new CalloutTitle(title) }))
      continue
    }
    if (inCallout) {
      const closing = /^:{3,}\s*$/u.test(t)
      push(line.from, line.from, Decoration.line({ class: closing ? 'cm-callout cm-callout-fence' : 'cm-callout' }))
      if (closing) {
        if (!revealed(state, line.from, line.to)) push(line.from, line.to, hide)
        inCallout = false
      }
    }
  }

  const fm = frontMatterRange(state)
  if (fm && !revealed(state, fm.from, fm.to)) {
    push(fm.from, fm.to, Decoration.replace({ widget: new FrontMatterWidget(frontMatterSummary(fm.body)), block: true }))
  }

  // `^id` block markers are addresses for `![[note#^id]]`, not prose: off the cursor they go.
  // One at the end of a line takes the space before it along; one alone on its line — naming
  // the block above — takes the whole line, or an empty line would be left where it stood.
  const tree = syntaxTree(state)
  for (let ln = fm ? state.doc.lineAt(fm.to).number + 1 : 1; ln <= state.doc.lines; ln++) {
    const line = state.doc.line(ln)
    const mark = blockMarker(line.text)
    if (!mark || revealed(state, line.from, line.to)) continue
    let code = false
    for (let n: SyntaxNode | null = tree.resolveInner(line.from + mark.to - 1, -1); n; n = n.parent) {
      if (/Code|Math|HTML|Comment/u.test(n.name)) code = true
    }
    if (code) continue
    if (mark.alone && ln > 1) push(state.doc.line(ln - 1).to, line.to, hide)
    else push(line.from + mark.from, line.from + mark.to, hide)
  }

  {
    syntaxTree(state).iterate({
      enter: (node) => {
        const n = node.node
        switch (node.name) {
          case 'ATXHeading1':
          case 'ATXHeading2':
          case 'ATXHeading3':
          case 'ATXHeading4':
          case 'ATXHeading5':
          case 'ATXHeading6': {
            const level = node.name.slice(-1)
            push(node.from, node.from, Decoration.line({ class: `cm-heading cm-h${level}` }))
            if (!revealed(state, node.from, node.to)) {
              const mark = n.getChild('HeaderMark')
              if (mark) push(mark.from, Math.min(mark.to + 1, node.to), hide)
            }
            break
          }
          case 'Emphasis':
          case 'StrongEmphasis':
          case 'Strikethrough':
          case 'InlineCode':
            if (!revealed(state, node.from, node.to)) hideMarks(n, push)
            break
          case 'Link': {
            if (revealed(state, node.from, node.to)) break
            // [text](url "title") → keep text, hide the rest
            const marks = n.getChildren('LinkMark')
            const url = n.getChild('URL')
            if (marks.length >= 2 && url) {
              push(marks[0]!.from, marks[0]!.to, hide)
              push(marks[1]!.from, node.to, hide)
            }
            break
          }
          case 'Image': {
            if (revealed(state, node.from, node.to)) break
            const url = n.getChild('URL')
            if (url) {
              const target = state.sliceDoc(url.from, url.to)
              const src = opts.embedUrl(target) ?? target
              push(node.from, node.to, Decoration.replace({ widget: new ImageWidget(src, ''), block: false }))
            }
            break
          }
          case 'WikiLink': {
            const text = state.sliceDoc(node.from + 2, node.to - 2)
            const [targetPart, label] = text.split('|', 2)
            const target = targetPart!.split('#')[0]!.trim()
            if (!revealed(state, node.from, node.to)) {
              push(node.from, node.to, Decoration.replace({ widget: new LinkWidget((label ?? targetPart!).trim(), target, opts.openLink) }))
            } else {
              push(node.from, node.to, Decoration.mark({ class: 'cm-wikilink-src' }))
            }
            break
          }
          case 'WikiEmbed': {
            if (revealed(state, node.from, node.to)) break
            const text = state.sliceDoc(node.from + 3, node.to - 2)
            const target = text.split('|')[0]!.trim()
            const url = opts.embedUrl(target)
            if (url) {
              push(node.from, node.to, Decoration.replace({ widget: new ImageWidget(url, target) }))
              break
            }
            // A note is a block of its own, so only an embed alone on its line becomes one;
            // one in the middle of a sentence stays a link. A block widget covers whole lines.
            const line = state.doc.lineAt(node.from)
            const alone = line.text.trim() === state.sliceDoc(node.from, node.to)
            const parsed = parseEmbed(text)
            const embed = alone ? opts.embedNote?.(parsed) : undefined
            if (embed) {
              push(line.from, line.to, Decoration.replace({ widget: new TranscludeWidget(embed, parsed.note, opts.openLink), block: true }))
            } else {
              push(node.from, node.to, Decoration.replace({ widget: new LinkWidget(`![[${target}]]`, parsed.note, opts.openLink) }))
            }
            break
          }
          case 'NoteTag':
            push(node.from, node.to, Decoration.mark({ class: 'cm-tag' }))
            break
          case 'InlineMath': {
            if (revealed(state, node.from, node.to)) break
            const tex = state.sliceDoc(node.from + 1, node.to - 1)
            push(node.from, node.to, Decoration.replace({ widget: new MathWidget(tex, false) }))
            break
          }
          case 'BlockMath': {
            if (revealed(state, node.from, node.to)) break
            const raw = state.sliceDoc(node.from, node.to).trim()
            const tex = raw.replace(/^\$\$/u, '').replace(/\$\$$/u, '').trim()
            push(node.from, node.to, Decoration.replace({ widget: new MathWidget(tex, true), block: true }))
            break
          }
          case 'ListMark': {
            // Bullets only; an ordered list is numbered as a whole, under `OrderedList` below.
            const item = n.parent
            const list = item?.parent
            if (!item || list?.name !== 'BulletList' || revealed(state, node.from, node.to)) break
            let depth = 0
            for (let p: SyntaxNode | null = list; p; p = p.parent) if (p.name === 'BulletList') depth++
            // A task item wraps its content in `Task`, which is what holds the `[ ]` marker.
            const bullet = listBullet(depth, item.getChild('Task') !== null)
            push(node.from, node.to, Decoration.replace({ widget: new MarkerWidget(bullet, 'cm-list-bullet') }))
            break
          }
          case 'OrderedList': {
            // Numbered together, because an item's number is its *position*: markdown counts
            // from the first item's number and ignores the digits after it, which is what lets
            // `1.` on every line work — and what makes an item's number right again the moment
            // it is indented away, with nothing rewritten in the file.
            let depth = 0
            for (let p: SyntaxNode | null = n; p; p = p.parent) if (p.name === 'OrderedList') depth++
            let start: number | null = null
            let index = 0
            for (const li of n.getChildren('ListItem')) {
              const mark = li.getChild('ListMark')
              if (!mark) continue
              const m = /^(\d+)([.)])$/u.exec(state.sliceDoc(mark.from, mark.to))
              if (!m) continue
              start ??= Number(m[1])
              const label = listNumber(depth, start + index++)
              if (label === null || revealed(state, mark.from, mark.to)) continue
              const text = label + m[2]!
              if (text === m[0]) continue // already what the file says: nothing to draw
              push(mark.from, mark.to, Decoration.replace({ widget: new MarkerWidget(text, 'cm-list-number') }))
            }
            break
          }
          case 'TaskMarker': {
            if (revealed(state, node.from, node.to)) break
            const checked = /x/iu.test(state.sliceDoc(node.from, node.to))
            push(node.from, node.to, Decoration.replace({ widget: new CheckboxWidget(checked) }))
            break
          }
          case 'Blockquote': {
            // One line class per line of the quote; hide the `> ` markers unless revealed.
            const fromLine = state.doc.lineAt(node.from).number
            const toLine = state.doc.lineAt(node.to).number
            for (let ln = fromLine; ln <= toLine; ln++) push(state.doc.line(ln).from, state.doc.line(ln).from, Decoration.line({ class: 'cm-blockquote' }))
            if (!revealed(state, node.from, node.to)) {
              for (const m of n.getChildren('QuoteMark')) push(m.from, Math.min(m.to + 1, state.doc.lineAt(m.from).to), hide)
            }
            break
          }
          case 'FencedCode': {
            const fromLine = state.doc.lineAt(node.from).number
            const toLine = state.doc.lineAt(node.to).number
            // Off the cursor the fences fold away: the opening one to the language's name, the
            // closing one to nothing. Each keeps its line, as a band above and below the code.
            const folded = !revealed(state, node.from, node.to)
            const marks = n.getChildren('CodeMark')
            const open = marks[0]
            const close = marks.length > 1 ? marks[marks.length - 1] : undefined
            for (let ln = fromLine; ln <= toLine; ln++) {
              const line = state.doc.line(ln)
              const opening = ln === fromLine
              const closing = !opening && close !== undefined && state.doc.lineAt(close.from).number === ln
              const cls = opening || closing ? `cm-codeblock cm-codeblock-fence${folded ? ' cm-codeblock-folded' : ''}` : 'cm-codeblock'
              push(line.from, line.from, Decoration.line({ class: cls }))
            }
            if (folded && open) {
              const info = n.getChild('CodeInfo')
              const lang = info ? codeLanguageName(state.sliceDoc(info.from, info.to)) : ''
              const end = state.doc.lineAt(open.from).to
              push(open.from, end, lang ? Decoration.replace({ widget: new MarkerWidget(lang, 'cm-codeblock-lang') }) : hide)
            }
            if (folded && close) push(close.from, close.to, hide)
            break
          }
          case 'Table': {
            const fromLine = state.doc.lineAt(node.from).number
            const toLine = state.doc.lineAt(node.to).number
            if (revealed(state, node.from, node.to)) {
              for (let ln = fromLine; ln <= toLine; ln++) push(state.doc.line(ln).from, state.doc.line(ln).from, Decoration.line({ class: 'cm-table-row' }))
              break
            }
            // A block widget has to cover whole lines; a table's node starts after any indent.
            const from = state.doc.line(fromLine).from
            const to = state.doc.line(toLine).to
            const doc = (f: number, t: number) => state.sliceDoc(f, t)
            const model = tableModel(doc, n, from)
            push(from, to, Decoration.replace({ widget: new TableWidget(state.sliceDoc(from, to), model, opts.openLink), block: true }))
            break
          }
          default:
            break
        }
      },
    })
  }
  // Replacements may not overlap each other (nested markup: the outer one wins); marks and
  // line decorations may. `Decoration.set(…, true)` sorts by position and side for us.
  items.sort((a, b) => a.from - b.from || b.to - a.to)
  const ranges = []
  let replacedUntil = -1
  for (const it of items) {
    const isReplace = it.deco.spec.widget !== undefined || it.deco.spec.block !== undefined || it.deco === hide
    if (isReplace) {
      if (it.from < replacedUntil) continue
      replacedUntil = Math.max(replacedUntil, it.to)
    }
    ranges.push(it.deco.range(it.from, it.to))
  }
  return Decoration.set(ranges, true)
}

function hideMarks(n: SyntaxNode, push: (f: number, t: number, d: Decoration) => void) {
  for (const name of ['EmphasisMark', 'CodeMark', 'StrikethroughMark']) {
    for (const m of n.getChildren(name)) push(m.from, m.to, hide)
  }
}

export function livePreview(opts: LivePreviewOptions) {
  // A StateField rather than a ViewPlugin: block-level replacements (math blocks, folded
  // front matter) are only allowed from fields. Recomputed on document or selection changes.
  const field = StateField.define<DecorationSet>({
    create: (state) => build(state, opts),
    update: (deco, tr) =>
      tr.docChanged || tr.selection || tr.effects.some((e) => e.is(refreshPreview)) ? build(tr.state, opts) : deco,
    provide: (f) => EditorView.decorations.from(f),
  })
  return [
    field,
    // Toggle task checkboxes by clicking the rendered box.
    EditorView.domEventHandlers({
      mousedown(event, view) {
        const target = event.target as HTMLElement
        if (!target.classList?.contains('cm-task-checkbox')) return false
        const pos = view.posAtDOM(target)
        const line = view.state.doc.lineAt(pos)
        const m = /^(\s*(?:[-*+]|\d+[.)])\s+\[)( |x|X)(\])/u.exec(line.text)
        if (!m) return false
        const at = line.from + m[1]!.length
        view.dispatch({ changes: { from: at, to: at + 1, insert: m[2] === ' ' ? 'x' : ' ' } })
        event.preventDefault()
        return true
      },
    }),
  ]
}
