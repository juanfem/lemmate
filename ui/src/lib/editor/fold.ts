// Folding beyond what the markdown language gives: an HTML comment folds whole — `<!--…-->`,
// not its first line with the rest hidden — and the palette folds every comment or every
// section at once. Sections themselves fold already: lang-markdown's `headerIndent` service
// makes a heading line hide everything up to the next heading of its level or above.
//
// The arrow that folds them sits beside the text, not in a gutter at the pane's edge: with the
// text centred, a gutter put it a hand's width away from the heading it belonged to.
import { RangeSetBuilder, type EditorState, type StateEffect } from '@codemirror/state'
import { Decoration, EditorView, ViewPlugin, WidgetType, type DecorationSet, type ViewUpdate } from '@codemirror/view'
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

/** Whether the line at `from`..`to` opens a heading or a comment — the lines that get an arrow.
 *  A paragraph or a table folds too (from the keyboard), but an arrow beside every one of them
 *  would be clutter in the text itself. */
function arrowLine(state: EditorState, from: number, to: number): boolean {
  let found = false
  syntaxTree(state).iterate({
    from,
    to,
    enter: (n) => {
      if (found || n.name === 'FencedCode' || n.name === 'CodeBlock') return false
      if (n.from < from) return
      if (isHeading(n.name) || ((n.name === 'CommentBlock' || n.name === 'Comment') && commentRange(state, n.from, n.to))) found = true
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

class FoldArrow extends WidgetType {
  readonly range: Range
  readonly open: boolean
  constructor(range: Range, open: boolean) {
    super()
    this.range = range
    this.open = open
  }
  eq(other: FoldArrow) {
    return other.range.from === this.range.from && other.range.to === this.range.to && other.open === this.open
  }
  toDOM(view: EditorView) {
    // A zero-width anchor at the start of the line, so the text does not move, and the arrow
    // hung off it into the margin to its left.
    const wrap = document.createElement('span')
    wrap.className = `cm-fold-arrow${this.open ? '' : ' cm-fold-arrow-closed'}`
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

/** Where the arrows go in `from`..`to`: the start of each heading or comment line that folds,
 *  with the range it folds and whether that is open. */
export function arrowsIn(state: EditorState, from: number, to: number): { at: number; range: Range; open: boolean }[] {
  const out: { at: number; range: Range; open: boolean }[] = []
  for (let pos = from; pos <= to; ) {
    const line = state.doc.lineAt(pos)
    const range = arrowLine(state, line.from, line.to) ? foldable(state, line.from, line.to) : null
    if (range) out.push({ at: line.from, range, open: !closedFold(state, range) })
    pos = line.to + 1
  }
  return out
}

function arrows(view: EditorView): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>()
  for (const { from, to } of view.visibleRanges) {
    for (const { at, range, open } of arrowsIn(view.state, from, to)) {
      builder.add(at, at, Decoration.widget({ widget: new FoldArrow(range, open), side: -1 }))
    }
  }
  return builder.finish()
}

/** The arrow beside each heading and comment that folds: shown while the pointer is on its
 *  line, and all the time once it is folded, so a folded section says so where it starts. */
export const foldArrows = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet
    constructor(view: EditorView) {
      this.decorations = arrows(view)
    }
    update(u: ViewUpdate) {
      if (
        u.docChanged ||
        u.viewportChanged ||
        syntaxTree(u.startState) !== syntaxTree(u.state) ||
        foldedRanges(u.startState) !== foldedRanges(u.state)
      ) {
        this.decorations = arrows(u.view)
      }
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
