// Folding beyond what the markdown language gives: an HTML comment folds whole — `<!--…-->`,
// not its first line with the rest hidden — and the palette folds every comment or every
// section at once. Sections themselves fold already: lang-markdown's `headerIndent` service
// makes a heading line hide everything up to the next heading of its level or above.
//
// The arrow that folds them sits beside the text, not in a gutter at the pane's edge: with the
// text centred, a gutter put it a hand's width away from the heading it belonged to.
import { type EditorState, type StateEffect } from '@codemirror/state'
import { Decoration, EditorView, ViewPlugin, WidgetType, type DecorationSet, type ViewUpdate } from '@codemirror/view'
import type { SyntaxNode } from '@lezer/common'
import { ensureSyntaxTree, foldable, foldedRanges, foldEffect, foldService, syntaxTree, unfoldEffect } from '@codemirror/language'

type Range = { from: number; to: number }

/** What folding comment `from`..`to` hides: everything between `<!--` and `-->`. A comment
 *  left open runs to the end of its node, as the parser read it. */
function commentRange(state: EditorState, from: number, to: number): Range | null {
  const text = state.sliceDoc(from, to)
  if (!text.startsWith('<!--')) return null
  const close = text.indexOf('-->', 4)
  const range = { from: from + 4, to: close < 0 ? to : from + close }
  return range.to > range.from ? range : null
}

/** Every HTML comment in `from`..`to`, as the range folding it hides. Code is not looked into:
 *  a fenced block's grammar has comments of its own, and those are its business. */
export function commentRanges(state: EditorState, from = 0, to = state.doc.length): Range[] {
  const tree = ensureSyntaxTree(state, to, 200) ?? syntaxTree(state)
  const out: Range[] = []
  tree.iterate({
    from,
    to,
    enter: (n) => {
      if (n.name === 'FencedCode' || n.name === 'CodeBlock') return false
      if (n.name !== 'CommentBlock' && n.name !== 'Comment') return
      const range = commentRange(state, n.from, n.to)
      if (range && !out.some((r) => r.from === range.from)) out.push(range)
      return false
    },
  })
  return out
}

/** A comment that opens on this line and runs past it folds from here. */
export const commentFolding = foldService.of((state, lineStart, lineEnd) => {
  const range = commentRanges(state, lineStart, lineEnd).find((r) => r.from - 4 >= lineStart && r.from - 4 < lineEnd)
  return range && range.to > lineEnd ? range : null
})

const isHeading = (name: string) => /^(?:ATX|Setext)Heading\d$/u.test(name)

const DIV_OPEN = /^:{3,}\s*[^\s:]/u
const DIV_CLOSE = /^:{3,}\s*$/u

/** Is `pos` in code or front matter, where a `:::` line is text and opens nothing? */
function inCodeOrFrontMatter(state: EditorState, pos: number): boolean {
  for (let n: SyntaxNode | null = syntaxTree(state).resolveInner(pos, 1); n; n = n.parent) {
    if (n.name === 'FencedCode' || n.name === 'CodeBlock' || n.name === 'FrontMatter') return true
  }
  return false
}

/** A pandoc fenced div — `::: notes`, a Quarto callout, any `::: {…}` — folds from its opening
 *  line to its closing fence. They nest, a `:::` in a code block is code, and one never closed
 *  folds nothing (pandoc reads it as text). */
export const divFolding = foldService.of((state, lineStart, lineEnd) => {
  const first = state.doc.lineAt(lineStart)
  if (!DIV_OPEN.test(first.text.trim()) || inCodeOrFrontMatter(state, lineStart)) return null
  let depth = 0
  let fence: { ch: string; len: number } | null = null
  for (let ln = first.number + 1; ln <= state.doc.lines; ln++) {
    const line = state.doc.line(ln)
    const t = line.text.trim()
    if (fence) {
      const m = /^(`{3,}|~{3,})\s*$/u.exec(t)
      if (m && m[1]![0] === fence.ch && m[1]!.length >= fence.len) fence = null
      continue
    }
    const f = /^(`{3,}|~{3,})/u.exec(t)
    if (f && !(f[1]![0] === '`' && t.slice(f[1]!.length).includes('`'))) {
      fence = { ch: f[1]![0]!, len: f[1]!.length }
      continue
    }
    if (DIV_CLOSE.test(t)) {
      if (depth === 0) return { from: lineEnd, to: line.to }
      depth--
    } else if (DIV_OPEN.test(t)) depth++
  }
  return null
})

/** Whether the line at `from`..`to` opens a block that gets an arrow: a heading, a comment, a
 *  code block, a quote or a fenced div. A paragraph folds too (from the keyboard), but an arrow
 *  for every one would be clutter; a table and display math are drawn as widgets in live
 *  preview, which a fold would fight with. */
function arrowLine(state: EditorState, from: number, to: number): boolean {
  if (DIV_OPEN.test(state.sliceDoc(from, to).trim())) return true
  let found = false
  syntaxTree(state).iterate({
    from,
    to,
    enter: (n) => {
      if (found) return false
      const code = n.name === 'FencedCode' || n.name === 'CodeBlock'
      if (n.from < from) return code ? false : undefined
      if (isHeading(n.name) || n.name === 'FencedCode' || n.name === 'Blockquote') found = true
      else if ((n.name === 'CommentBlock' || n.name === 'Comment') && commentRange(state, n.from, n.to)) found = true
      if (code) return false
    },
  })
  return found
}

function folded(state: EditorState): Range[] {
  const out: Range[] = []
  foldedRanges(state).between(0, state.doc.length, (from, to) => void out.push({ from, to }))
  return out
}

/** The fold `range` is closed in, if it is. */
function closedFold(state: EditorState, range: Range): Range | undefined {
  return folded(state).find((f) => f.from === range.from && f.to === range.to)
}

/** The `range.from` of the arrow the pointer is over the block of, per editor. */
const pointedAt = new WeakMap<EditorView, number>()

class FoldArrow extends WidgetType {
  readonly range: Range
  readonly open: boolean
  readonly here: boolean
  constructor(range: Range, open: boolean, here: boolean) {
    super()
    this.range = range
    this.open = open
    this.here = here
  }
  eq(other: FoldArrow) {
    return other.range.from === this.range.from && other.range.to === this.range.to && other.open === this.open && other.here === this.here
  }
  toDOM(view: EditorView) {
    // A zero-width anchor at the start of the line, so the text does not move: it says where
    // the line's baseline is, and the arrow hangs off it into the margin (setup.ts).
    const wrap = document.createElement('span')
    wrap.className = 'cm-fold-arrow'
    wrap.classList.toggle('cm-fold-arrow-closed', !this.open)
    wrap.classList.toggle('cm-fold-arrow-here', this.here)
    wrap.classList.toggle('cm-fold-arrow-pointed', pointedAt.get(view) === this.range.from)
    wrap.dataset.from = String(this.range.from)
    const button = wrap.appendChild(document.createElement('span'))
    // A chevron rather than ▾/▸, which most fonts draw at half the size of the text around them.
    button.innerHTML = '<svg viewBox="0 0 16 16" width="12" height="12"><path d="M6 3.5 10.5 8 6 12.5" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>'
    button.title = this.open ? 'Fold' : 'Unfold'
    button.setAttribute('aria-hidden', 'true')
    // On mousedown, before CodeMirror moves the cursor there — which would reveal the line's
    // markup in live preview and jump the arrow along with it.
    button.addEventListener('mousedown', (e) => {
      e.preventDefault()
      const closed = closedFold(view.state, this.range)
      view.dispatch({ effects: closed ? unfoldEffect.of(closed) : foldEffect.of(this.range) })
    })
    return wrap
  }
  ignoreEvent() {
    return true
  }
}

type Arrow = { at: number; range: Range; open: boolean }

/** The line an arrow hangs off, which places it: off the line's own left edge — outside a
 *  quote's or a callout's border, not on it. */
const foldLine = Decoration.line({ class: 'cm-fold-line' })

/** Where the arrows go in `from`..`to`: the start of each line opening a block that folds,
 *  with the range it folds and whether that is open. */
export function arrowsIn(state: EditorState, from: number, to: number): Arrow[] {
  const out: Arrow[] = []
  for (let pos = from; pos <= to; ) {
    const line = state.doc.lineAt(pos)
    const range = arrowLine(state, line.from, line.to) ? foldable(state, line.from, line.to) : null
    if (range) out.push({ at: line.from, range, open: !closedFold(state, range) })
    pos = line.to + 1
  }
  return out
}

/** The arrow of the innermost block holding `pos` — its opening line through what it folds —
 *  so inside a subsection it is the subsection's, not the chapter's. */
export function arrowFor(arrows: Arrow[], pos: number): Arrow | undefined {
  let best: Arrow | undefined
  for (const a of arrows) if (a.at <= pos && pos <= a.range.to && (!best || a.at > best.at)) best = a
  return best
}

/** The arrow beside each block that folds. It shows for the block the pointer is in and the
 *  one the cursor is in — the innermost, both — and all the time once folded, so a folded
 *  block says so where it starts. */
export const foldArrows = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet
    arrows: Arrow[] = []
    readonly view: EditorView
    constructor(view: EditorView) {
      this.view = view
      this.decorations = this.build(view.state)
      view.dom.addEventListener('mousemove', this.move)
      view.dom.addEventListener('mouseleave', this.leave)
    }
    build(state: EditorState): DecorationSet {
      // A fold splits the visible ranges, and the line it ends on can open both halves.
      const seen = new Set<number>()
      this.arrows = this.view.visibleRanges
        .flatMap(({ from, to }) => arrowsIn(state, from, to))
        .filter((a) => !seen.has(a.at) && seen.add(a.at))
      const here = arrowFor(this.arrows, state.selection.main.head)
      return Decoration.set(
        this.arrows.flatMap((a) => [foldLine.range(a.at), Decoration.widget({ widget: new FoldArrow(a.range, a.open, a === here), side: -1 }).range(a.at)]),
        true,
      )
    }
    update(u: ViewUpdate) {
      const pointed = pointedAt.get(u.view)
      if (pointed !== undefined && u.docChanged) pointedAt.set(u.view, u.changes.mapPos(pointed))
      if (
        u.docChanged ||
        u.viewportChanged ||
        u.selectionSet ||
        syntaxTree(u.startState) !== syntaxTree(u.state) ||
        foldedRanges(u.startState) !== foldedRanges(u.state)
      ) {
        this.decorations = this.build(u.state)
      }
    }
    point(from: number | undefined) {
      if (pointedAt.get(this.view) === from) return
      if (from === undefined) pointedAt.delete(this.view)
      else pointedAt.set(this.view, from)
      for (const el of this.view.contentDOM.querySelectorAll<HTMLElement>('.cm-fold-arrow')) {
        el.classList.toggle('cm-fold-arrow-pointed', el.dataset.from === String(from))
      }
    }
    move = (e: MouseEvent) => {
      const pos = this.view.posAtCoords({ x: e.clientX, y: e.clientY }, false)
      this.point(arrowFor(this.arrows, pos)?.range.from)
    }
    leave = () => this.point(undefined)
    destroy() {
      this.view.dom.removeEventListener('mousemove', this.move)
      this.view.dom.removeEventListener('mouseleave', this.leave)
    }
  },
  { decorations: (p) => p.decorations },
)

function dispatch(view: EditorView, effects: StateEffect<unknown>[]): boolean {
  if (effects.length) view.dispatch({ effects })
  return effects.length > 0
}

/** The effects folding every comment that is not folded yet — one-line comments too: a long
 *  aside is as much in the way on one line as on five. */
export function foldCommentsEffects(state: EditorState): StateEffect<unknown>[] {
  return commentRanges(state)
    .filter((r) => !closedFold(state, r))
    .map((r) => foldEffect.of(r))
}

/** …and unfolding every folded comment, leaving folded sections alone. */
export function unfoldCommentsEffects(state: EditorState): StateEffect<unknown>[] {
  const comments = commentRanges(state)
  return folded(state)
    .filter((f) => comments.some((r) => f.from === r.from && f.to === r.to))
    .map((f) => unfoldEffect.of(f))
}

/** The effects folding every heading's section, nested ones included, so unfolding a chapter
 *  shows its sections still folded — an outline of it. */
export function foldSectionsEffects(state: EditorState): StateEffect<unknown>[] {
  const out: StateEffect<unknown>[] = []
  ;(ensureSyntaxTree(state, state.doc.length, 200) ?? syntaxTree(state)).iterate({
    enter: (n) => {
      if (n.name === 'FencedCode' || n.name === 'CodeBlock' || n.name === 'FrontMatter') return false
      if (!isHeading(n.name)) return
      const line = state.doc.lineAt(n.from)
      const range = foldable(state, line.from, line.to)
      if (range && !closedFold(state, range)) out.push(foldEffect.of(range))
      return false
    },
  })
  return out
}

export const foldComments = (view: EditorView) => dispatch(view, foldCommentsEffects(view.state))
export const unfoldComments = (view: EditorView) => dispatch(view, unfoldCommentsEffects(view.state))
export const foldSections = (view: EditorView) => dispatch(view, foldSectionsEffects(view.state))
