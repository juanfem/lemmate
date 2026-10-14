// What the spell checker should leave alone. The editor asks the browser to check the note
// (`lib/spellcheck.ts`), but a URL, a `[[link]]`'s note name, a `#tag` or a piece of code is not
// prose, and underlining it only buries the real mistakes. Those spans get `spellcheck="false"`,
// which browsers and webviews honour on an element inside an editable one.
//
// A `[label](url)` keeps its label checked: only the address is skipped.
import { Decoration, type DecorationSet, type EditorView, ViewPlugin, type ViewUpdate } from '@codemirror/view'
import { type EditorState, RangeSetBuilder } from '@codemirror/state'
import { syntaxTree } from '@codemirror/language'

/** Syntax nodes whose whole text is skipped — the children too, which are never prose. */
const SKIP = new Set(['URL', 'Autolink', 'WikiLink', 'WikiEmbed', 'NoteTag', 'InlineCode', 'FencedCode', 'CodeBlock'])

/** The spans between `from` and `to` that are not checked, in order and not overlapping. */
export function uncheckedSpans(state: EditorState, from = 0, to = state.doc.length): { from: number; to: number }[] {
  const out: { from: number; to: number }[] = []
  syntaxTree(state).iterate({
    from,
    to,
    enter: (n) => {
      if (!SKIP.has(n.name)) return true
      if (n.to > n.from) out.push({ from: n.from, to: n.to })
      return false
    },
  })
  return out
}

const unchecked = Decoration.mark({ attributes: { spellcheck: 'false' } })

function build(view: EditorView): DecorationSet {
  const b = new RangeSetBuilder<Decoration>()
  for (const { from, to } of view.visibleRanges) {
    for (const s of uncheckedSpans(view.state, from, to)) b.add(s.from, s.to, unchecked)
  }
  return b.finish()
}

export const skipSpelling = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet
    constructor(view: EditorView) {
      this.decorations = build(view)
    }
    update(u: ViewUpdate) {
      if (u.docChanged || u.viewportChanged || syntaxTree(u.startState) !== syntaxTree(u.state)) this.decorations = build(u.view)
    }
  },
  { decorations: (p) => p.decorations },
)
