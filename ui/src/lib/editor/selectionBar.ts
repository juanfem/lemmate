// The floating format bar (SPEC §8): a row of buttons over a selection, for the reader who
// does not know — or does not want — the markdown for bold. It appears once the selection is
// made (not while the mouse is still dragging it out), and never where the text cannot be
// edited: reading mode, or a note shared read-only.
//
// A phone has its own row above the keyboard, and its own selection handles and menu that a
// bar over the text would fight with, so on a coarse pointer there is no floating bar at all.

import { StateEffect, StateField, type EditorState, type Extension } from '@codemirror/state'
import { EditorView, ViewPlugin, showTooltip, type Tooltip, type TooltipView } from '@codemirror/view'
import { hasMark, insertLink, run, toggleMark, toggleWikilink, type Mark, type Plan } from './format.ts'

export const MOD = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/u.test(navigator.platform) ? '⌘' : 'Ctrl+'

interface Button {
  label: string
  title: string
  plan: Plan
  mark?: Mark
  className?: string
}

const BUTTONS: Button[] = [
  { label: 'B', title: `Bold (${MOD}B)`, plan: toggleMark('**'), mark: '**', className: 'b' },
  { label: 'I', title: `Italic (${MOD}I)`, plan: toggleMark('*'), mark: '*', className: 'i' },
  { label: 'S', title: `Strikethrough (${MOD}Shift+X)`, plan: toggleMark('~~'), mark: '~~', className: 's' },
  { label: '</>', title: 'Code', plan: toggleMark('`'), mark: '`', className: 'code' },
  { label: 'Link', title: `Link (${MOD}Shift+K)`, plan: insertLink },
  { label: '[[ ]]', title: 'Link to a note', plan: toggleWikilink },
]

/** Whether a mouse button is held in the text: the bar waits for the selection to be finished. */
const setDragging = StateEffect.define<boolean>()
const dragging = StateField.define<boolean>({
  create: () => false,
  update: (v, tr) => tr.effects.reduce((v, e) => (e.is(setDragging) ? e.value : v), v),
})

function createBar(view: EditorView): TooltipView {
  const dom = document.createElement('div')
  dom.className = 'cm-format-bar'
  dom.setAttribute('role', 'toolbar')
  dom.setAttribute('aria-label', 'Format')
  // A press must not take the focus, or the selection, away from the text it is about to format.
  dom.addEventListener('mousedown', (e) => e.preventDefault())
  const marked: [HTMLButtonElement, Mark][] = []
  for (const b of BUTTONS) {
    const btn = document.createElement('button')
    btn.type = 'button'
    btn.textContent = b.label
    btn.title = b.title
    btn.setAttribute('aria-label', b.title)
    if (b.className) btn.classList.add(b.className)
    btn.addEventListener('click', () => {
      run(b.plan)(view)
      view.focus()
    })
    if (b.mark) marked.push([btn, b.mark])
    dom.append(btn)
  }
  const light = (state: EditorState) => {
    for (const [btn, mark] of marked) btn.setAttribute('aria-pressed', String(hasMark(state, mark)))
  }
  light(view.state)
  return { dom, update: (u) => (u.docChanged || u.selectionSet) && light(u.state) }
}

function barFor(state: EditorState): readonly Tooltip[] {
  const sel = state.selection.main
  if (sel.empty || state.field(dragging) || state.readOnly || !state.facet(EditorView.editable)) return []
  return [{ pos: sel.from, above: true, create: createBar }]
}

const bar = StateField.define<readonly Tooltip[]>({
  create: barFor,
  update(tips, tr) {
    const pressed = tr.effects.some((e) => e.is(setDragging))
    return tr.docChanged || tr.selection || pressed || tr.reconfigured ? barFor(tr.state) : tips
  },
  provide: (f) => showTooltip.computeN([f], (s) => s.field(f)),
})

/** Hold the bar back while a mouse button is down in the text; show it on release. */
const pointer = ViewPlugin.fromClass(
  class {
    view: EditorView
    constructor(view: EditorView) {
      this.view = view
      document.addEventListener('mouseup', this.up)
    }
    up = () => {
      if (this.view.state.field(dragging)) this.view.dispatch({ effects: setDragging.of(false) })
    }
    destroy() {
      document.removeEventListener('mouseup', this.up)
    }
  },
  {
    eventHandlers: {
      mousedown(e) {
        if (e.button === 0) this.view.dispatch({ effects: setDragging.of(true) })
      },
    },
  },
)

const look = EditorView.baseTheme({
  '.cm-tooltip.cm-format-bar': {
    display: 'flex',
    gap: '2px',
    padding: '3px',
    border: '1px solid var(--border)',
    borderRadius: '8px',
    background: 'var(--panel)',
    boxShadow: '0 6px 20px rgb(0 0 0 / 0.18)',
    fontFamily: 'var(--ui)',
  },
  '.cm-format-bar button': {
    font: 'inherit',
    fontSize: '0.8rem',
    minWidth: '1.9rem',
    padding: '0.2rem 0.45rem',
    border: '0',
    borderRadius: '5px',
    background: 'none',
    color: 'inherit',
    cursor: 'pointer',
  },
  '.cm-format-bar button:hover': { background: 'var(--hover)' },
  '.cm-format-bar button[aria-pressed="true"]': { color: 'var(--accent)', background: 'var(--hover)' },
  '.cm-format-bar .b': { fontWeight: '700' },
  '.cm-format-bar .i': { fontStyle: 'italic', fontFamily: 'Georgia, serif' },
  '.cm-format-bar .s': { textDecoration: 'line-through' },
  '.cm-format-bar .code': { fontFamily: 'var(--mono)', fontSize: '0.75rem' },
})

export function selectionBar(): Extension {
  if (typeof matchMedia === 'function' && matchMedia('(pointer: coarse)').matches) return []
  return [dragging, bar, pointer, look]
}
