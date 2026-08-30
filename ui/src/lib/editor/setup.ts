// CodeMirror 6 editor bound to a Y.Text (SPEC §8): markdown + our syntax extensions, live
// preview, collaborative cursors, and the usual keymaps.

import { EditorView, keymap, drawSelection, highlightActiveLine, rectangularSelection } from '@codemirror/view'
import { EditorState, type Extension } from '@codemirror/state'
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
  '.cm-content': { maxWidth: '46rem', margin: '0 auto', padding: '0 2rem', caretColor: 'var(--fg)' },
  '&.cm-focused': { outline: 'none' },
  '.cm-line': { padding: '0' },
  '.cm-gutters': { background: 'transparent', border: 0, color: 'var(--muted)' },
  '.cm-foldGutter .cm-gutterElement': { cursor: 'pointer', opacity: 0.35 },
  '.cm-foldGutter:hover .cm-gutterElement': { opacity: 1 },
  '.cm-foldPlaceholder': { background: 'var(--accent-bg)', border: 0, color: 'var(--accent)', borderRadius: '4px', padding: '0 0.4em' },
  '.cm-heading': { lineHeight: '1.3', marginTop: '0.6em' },
  '.cm-h1': { fontSize: '1.9em' },
  '.cm-h2': { fontSize: '1.5em' },
  '.cm-h3': { fontSize: '1.25em' },
  '.cm-h4': { fontSize: '1.1em' },
  '.cm-blockquote': { borderLeft: '3px solid var(--accent)', paddingLeft: '0.75em', color: 'var(--muted)' },
  '.cm-frontmatter': { fontFamily: 'var(--ui)', fontSize: '0.8em', color: 'var(--muted)', background: 'var(--panel)', border: '1px solid var(--border)', borderRadius: '6px', padding: '0.3em 0.7em', margin: '0 0 1em', cursor: 'text' },
  '.cm-tag': { color: 'var(--accent)', background: 'var(--accent-bg)', borderRadius: '999px', padding: '0 0.4em' },
  '.cm-wikilink': { color: 'var(--accent)', textDecoration: 'none', cursor: 'pointer' },
  '.cm-wikilink:hover': { textDecoration: 'underline' },
  '.cm-wikilink-src': { color: 'var(--accent)' },
  '.cm-math-block': { display: 'block', textAlign: 'center', padding: '0.5em 0' },
  '.cm-embed-image': { maxWidth: '100%', display: 'block', margin: '0.5em 0' },
  '.cm-task-checkbox': { marginRight: '0.4em', verticalAlign: 'middle' },
  '.cm-ySelectionInfo': { fontFamily: 'var(--ui)', fontSize: '0.7em' },
  '.cm-tooltip.cm-tooltip-autocomplete': { fontFamily: 'var(--ui)', fontSize: '0.85em', border: '1px solid var(--border)', background: 'var(--panel)', borderRadius: '6px' },
  '.cm-tooltip-autocomplete ul li[aria-selected]': { background: 'var(--accent-bg)', color: 'inherit' },
  '.cm-callout': { borderLeft: '3px solid var(--accent)', background: 'var(--accent-bg)', paddingLeft: '0.75em' },
  '.cm-callout-title': { fontWeight: '600', fontFamily: 'var(--ui)', fontSize: '0.9em' },
  '.cm-callout-fence': { color: 'var(--muted)', fontSize: '0.8em' },
  '.cm-codeblock': { fontFamily: 'var(--mono)', fontSize: '0.9em', background: 'var(--code-bg)', paddingLeft: '0.75em', paddingRight: '0.75em' },
  '.cm-codeblock-fence': { color: 'var(--muted)', fontSize: '0.8em' },
  '.cm-table-row': { fontFamily: 'var(--mono)', fontSize: '0.9em' },
})

export interface EditorOptions extends LivePreviewOptions {
  extra?: Extension[]
  complete?: CompletionSources
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
      livePreview(opts),
      ...(opts.complete ? [noteCompletions(opts.complete)] : []),
      yCollab(text, awareness),
      keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...searchKeymap, ...historyKeymap, ...foldKeymap, indentWithTab]),
      ...(opts.extra ?? []),
    ],
  })
  return new EditorView({ state, parent })
}
