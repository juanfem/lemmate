// CodeMirror 6 editor bound to a Y.Text (SPEC §8): markdown + our syntax extensions, live
// preview, collaborative cursors, and the usual keymaps.

import { EditorView, keymap, drawSelection, highlightActiveLine, rectangularSelection } from '@codemirror/view'
import { Compartment, EditorState, type Extension } from '@codemirror/state'
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands'
import { searchKeymap, highlightSelectionMatches } from '@codemirror/search'
import { closeBrackets, closeBracketsKeymap } from '@codemirror/autocomplete'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { syntaxHighlighting, HighlightStyle, indentOnInput, bracketMatching, foldGutter, foldKeymap, codeFolding } from '@codemirror/language'
import { tags as t } from '@lezer/highlight'
import { yCollab } from 'y-codemirror.next'
import type * as Y from 'yjs'
import type { Awareness } from 'y-protocols/awareness'
import { noteSyntax } from './syntax.ts'
import { livePreview, type LivePreviewOptions } from './livePreview.ts'
import { listIndent } from './lists.ts'
import { noteCompletions, type CompletionSources } from './complete.ts'

const highlight = HighlightStyle.define([
  { tag: t.heading, fontWeight: '600' },
  { tag: t.emphasis, fontStyle: 'italic' },
  { tag: t.strong, fontWeight: 'bold' },
  { tag: t.strikethrough, textDecoration: 'line-through' },
  { tag: t.monospace, fontFamily: 'var(--mono)', background: 'var(--code-bg)', borderRadius: '3px' },
  { tag: t.link, color: 'var(--accent)' },
  { tag: t.url, color: 'var(--muted)' },
  { tag: t.processingInstruction, color: 'var(--muted)' },
  { tag: t.quote, color: 'var(--muted)' },
  { tag: t.contentSeparator, color: 'var(--muted)' },
])

const theme = EditorView.theme({
  '&': { height: '100%', fontSize: '16px' },
  '.cm-scroller': { fontFamily: 'var(--prose)', lineHeight: '1.6', padding: '1rem 0' },
  // The side padding is a comfortable margin on a monitor and half the line length on a
  // phone, so it shrinks with the viewport instead of staying a fixed 2rem.
  '.cm-content': { maxWidth: '46rem', margin: '0 auto', padding: '0 clamp(0.9rem, 4vw, 2rem)', caretColor: 'var(--fg)' },
  '&.cm-focused': { outline: 'none' },
  '.cm-line': { padding: '0' },
  '.cm-gutters': { background: 'transparent', border: 0, color: 'var(--muted)' },
  '.cm-foldGutter .cm-gutterElement': { cursor: 'pointer', opacity: 0.35 },
  '.cm-foldGutter:hover .cm-gutterElement': { opacity: 1 },
  '.cm-foldPlaceholder': { background: 'var(--accent-bg)', border: 0, color: 'var(--accent)', borderRadius: '4px', padding: '0 0.4em' },
  // Space above a heading is *padding*, never margin: CodeMirror's height map measures each
  // line with getBoundingClientRect(), which excludes margins, so a margin here desynchronises
  // the map from the DOM and every click, drag and Up/Down below it lands on the wrong line.
  '.cm-heading': { lineHeight: '1.3', paddingTop: '0.6em' },
  '.cm-h1': { fontSize: '1.9em' },
  '.cm-h2': { fontSize: '1.5em' },
  '.cm-h3': { fontSize: '1.25em' },
  '.cm-h4': { fontSize: '1.1em' },
  '.cm-blockquote': { borderLeft: '3px solid var(--accent)', paddingLeft: '0.75em', color: 'var(--muted)' },
  // The gap below the properties is padding on an unstyled wrapper rather than a margin on the
  // box itself, for the same measuring reason as `.cm-heading` above.
  '.cm-frontmatter': { paddingBottom: '1em' },
  '.cm-frontmatter-box': { fontFamily: 'var(--ui)', fontSize: '0.8em', color: 'var(--muted)', background: 'var(--panel)', border: '1px solid var(--border)', borderRadius: '6px', padding: '0.3em 0.7em', cursor: 'text' },
  '.cm-tag': { color: 'var(--accent)', background: 'var(--accent-bg)', borderRadius: '999px', padding: '0 0.4em' },
  '.cm-wikilink': { color: 'var(--accent)', textDecoration: 'none', cursor: 'pointer' },
  '.cm-wikilink:hover': { textDecoration: 'underline' },
  '.cm-wikilink-src': { color: 'var(--accent)' },
  '.cm-math-block': { display: 'block', textAlign: 'center', padding: '0.5em 0' },
  '.cm-embed-image': { maxWidth: '100%', display: 'block', padding: '0.5em 0' },
  // A fixed width for every shape, so the text after the marker lines up whatever the level's
  // bullet is — and so the widget takes the same room the `-` it replaces did.
  '.cm-list-bullet': { display: 'inline-block', width: '1ch', textAlign: 'center', color: 'var(--muted)' },
  // The number keeps its natural width — `viii.` is wider than `8.` — and the marker colour
  // the highlighter gave the digits it replaces.
  '.cm-list-number': { color: 'var(--muted)' },
  // Drawn rather than left to the browser: a default control takes its colours from the
  // platform's colour scheme, and an unchecked one is then a dark box on our dark background —
  // present, clickable, and all but invisible. These follow the theme's own tokens instead.
  '.cm-task-checkbox': {
    appearance: 'none',
    width: '0.95em',
    height: '0.95em',
    margin: '0 0.4em 0 0',
    verticalAlign: '-0.1em',
    border: '1.5px solid var(--muted)',
    borderRadius: '3px',
    background: 'var(--bg)',
    cursor: 'pointer',
  },
  '.cm-task-checkbox:hover': { borderColor: 'var(--accent)' },
  '.cm-task-checkbox:checked': {
    background: `var(--accent) url("data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'><path d='M3.5 8.5l3 3 6-6' fill='none' stroke='white' stroke-width='2.4' stroke-linecap='round' stroke-linejoin='round'/></svg>") center/0.8em no-repeat`,
    borderColor: 'var(--accent)',
  },
  '.cm-ySelectionInfo': { fontFamily: 'var(--ui)', fontSize: '0.7em' },
  '.cm-tooltip.cm-tooltip-autocomplete': { fontFamily: 'var(--ui)', fontSize: '0.85em', border: '1px solid var(--border)', background: 'var(--panel)', borderRadius: '6px' },
  '.cm-tooltip-autocomplete ul li[aria-selected]': { background: 'var(--accent-bg)', color: 'inherit' },
  '.cm-callout': { borderLeft: '3px solid var(--accent)', background: 'var(--accent-bg)', paddingLeft: '0.75em' },
  '.cm-callout-title': { fontWeight: '600', fontFamily: 'var(--ui)', fontSize: '0.9em' },
  '.cm-callout-fence': { color: 'var(--muted)', fontSize: '0.8em' },
  '.cm-codeblock': { fontFamily: 'var(--mono)', fontSize: '0.9em', background: 'var(--code-bg)', paddingLeft: '0.75em', paddingRight: '0.75em' },
  '.cm-codeblock-fence': { color: 'var(--muted)', fontSize: '0.8em' },
  '.cm-table-row': { fontFamily: 'var(--mono)', fontSize: '0.9em' },
  // Source mode drops the prose face along with the decorations: it is code, so it looks it.
  '&.cm-mode-source .cm-scroller': { fontFamily: 'var(--mono)', fontSize: '0.92em' },
  // Nothing in reading mode is editable, so the caret, the active line and the fold gutter
  // are all noise.
  '&.cm-mode-reading .cm-content': { caretColor: 'transparent' },
  '&.cm-mode-reading .cm-activeLine': { background: 'transparent' },
  '&.cm-mode-reading .cm-gutters': { display: 'none' },
})

/**
 * SPEC §8: `live` hides markup and renders it in place, revealing it again on the cursor's
 * line; `source` is the markdown itself, in the mono face, with nothing hidden; `reading`
 * renders everything and takes the keyboard away. All three are the same document and the
 * same decorations — there is no second renderer to drift out of step.
 */
export type ViewMode = 'live' | 'source' | 'reading'

export const VIEW_MODES: { id: ViewMode; label: string; hint: string }[] = [
  { id: 'live', label: 'Live', hint: 'Live preview — markup renders in place, and shows again on the line you are editing' },
  { id: 'source', label: 'Source', hint: 'Source — the markdown itself, nothing hidden' },
  { id: 'reading', label: 'Reading', hint: 'Reading — fully rendered and read-only' },
]

// The per-mode looks are classes on the editor rather than themes of their own: two themes
// styling `.cm-scroller` have equal specificity, and which one wins then comes down to the
// order the stylesheets happened to mount in. The rules live in `theme` below, behind `&.…`.
const sourceLook = EditorView.editorAttributes.of({ class: 'cm-mode-source' })
const readingLook = EditorView.editorAttributes.of({ class: 'cm-mode-reading' })

function modeExtensions(mode: ViewMode, opts: LivePreviewOptions): Extension {
  if (mode === 'source') return [sourceLook]
  if (mode === 'reading') return [livePreview({ ...opts, alwaysFolded: true }), EditorView.editable.of(false), readingLook]
  return [livePreview(opts)]
}

export interface EditorOptions extends LivePreviewOptions {
  extra?: Extension[]
  complete?: CompletionSources
  mode?: ViewMode
}

const modeCompartment = new Compartment()

/** Swap the view mode in place — the doc, the scroll position and the collab binding all stay. */
export function setViewMode(view: EditorView, mode: ViewMode, opts: LivePreviewOptions) {
  view.dispatch({ effects: modeCompartment.reconfigure(modeExtensions(mode, opts)) })
}

export function createEditor(parent: HTMLElement, text: Y.Text, awareness: Awareness, opts: EditorOptions): EditorView {
  // Start after the front matter (if any) so it opens folded and the cursor lands on prose.
  const initial = text.toString()
  let cursor = 0
  if (initial.startsWith('---\n')) {
    const close = initial.indexOf('\n---', 4)
    if (close !== -1) cursor = Math.min(initial.length, close + 4 + (initial[close + 4] === '\n' ? 1 : 0))
  }
  const state = EditorState.create({
    doc: initial,
    selection: { anchor: cursor },
    extensions: [
      history(),
      drawSelection(),
      rectangularSelection(),
      highlightActiveLine(),
      highlightSelectionMatches(),
      indentOnInput(),
      bracketMatching(),
      codeFolding({ placeholderText: '…' }),
      foldGutter({ openText: '▾', closedText: '▸' }),
      closeBrackets(),
      EditorView.lineWrapping,
      markdown({ base: markdownLanguage, extensions: noteSyntax, addKeymap: true }),
      syntaxHighlighting(highlight),
      theme,
      modeCompartment.of(modeExtensions(opts.mode ?? 'live', opts)),
      ...(opts.complete ? [noteCompletions(opts.complete)] : []),
      yCollab(text, awareness),
      // Before `indentWithTab`, which only knows about indent units: in a list, Tab means
      // "nest this under the item above", and the two are different columns. It declines
      // anywhere its rules do not apply, and the generic indent then runs as before.
      keymap.of([
        { key: 'Tab', run: listIndent(1), shift: listIndent(-1) },
        ...closeBracketsKeymap,
        ...defaultKeymap,
        ...searchKeymap,
        ...historyKeymap,
        ...foldKeymap,
        indentWithTab,
      ]),
      ...(opts.extra ?? []),
    ],
  })
  return new EditorView({ state, parent })
}
