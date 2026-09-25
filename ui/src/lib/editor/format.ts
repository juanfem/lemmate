// Formatting commands (SPEC §8): the floating bar over a selection, the editor's context menu,
// the phone's toolbar and the Mod-B/I keys all run these, so they agree on what "bold" does.
//
// They work on the markdown text, never on a rendered model: a toggle adds or removes marker
// characters and nothing else, so it cannot rewrite syntax it does not understand (§1.3). Each
// is a pure function of the state, returning the change to make, which is what the tests hold
// them to; `run` turns one into a CodeMirror command.

import { EditorSelection, type ChangeSpec, type EditorState, type SelectionRange, type TransactionSpec } from '@codemirror/state'
import type { EditorView } from '@codemirror/view'

/** The inline marks the bar toggles, by the characters that write them. */
export type Mark = '**' | '*' | '~~' | '`'

export type Plan = (state: EditorState) => TransactionSpec | null

/** Make a plan a command: dispatched as one undo step, `false` when there is nothing to do. */
export function run(plan: Plan) {
  return (view: EditorView): boolean => {
    if (view.state.readOnly) return false
    const spec = plan(view.state)
    if (!spec) return false
    view.dispatch({ ...spec, scrollIntoView: true, userEvent: 'input.format' })
    return true
  }
}

/** How many `c` in a row end at `pos` (`dir -1`) or start there (`dir 1`). */
function runOf(doc: string, pos: number, c: string, dir: 1 | -1): number {
  let n = 0
  if (dir < 0) while (pos - n - 1 >= 0 && doc[pos - n - 1] === c) n++
  else while (pos + n < doc.length && doc[pos + n] === c) n++
  return n
}

/**
 * Whether runs of `before` and `after` marker characters around some text close `mark` over
 * it. Asterisks need counting: `***x***` is bold *and* italic, `**x**` is bold but not
 * italic, so italic is on when the run is odd and bold when it is at least two.
 */
function closes(mark: Mark, before: number, after: number): boolean {
  if (mark === '*') return before % 2 === 1 && after % 2 === 1
  return before >= mark.length && after >= mark.length
}

/** The part of `[from, to)` inside a line, without the spaces a double-click drags along. */
function trimmed(doc: string, from: number, to: number): [number, number] {
  while (from < to && /\s/u.test(doc[from] ?? '')) from++
  while (to > from && /\s/u.test(doc[to - 1] ?? '')) to--
  return [from, to]
}

/** A selection split at line breaks: `**` cannot span a paragraph, so each line is wrapped alone. */
function segments(state: EditorState, range: SelectionRange): [number, number][] {
  const doc = state.doc.toString()
  const out: [number, number][] = []
  for (let pos = range.from; pos <= range.to; ) {
    const line = state.doc.lineAt(pos)
    const [from, to] = trimmed(doc, Math.max(pos, line.from), Math.min(range.to, line.to))
    if (to > from) out.push([from, to])
    pos = line.to + 1
  }
  return out
}

/**
 * Whether `mark` is on over `[from, to)`, and if so where the text it wraps starts and ends. The
 * markers may lie outside the selection, inside it at its edges, or one of each — which is
 * what a wrapped multi-line selection looks like, its first line's closing marker and its last
 * line's opening one falling inside.
 */
function markAt(doc: string, mark: Mark, from: number, to: number): [number, number] | null {
  const c = mark[0] as string
  const lead = runOf(doc, from, c, 1)
  if (lead >= to - from) return null
  const tail = runOf(doc, to, c, -1)
  if (!closes(mark, runOf(doc, from, c, -1) + lead, tail + runOf(doc, to, c, 1))) return null
  return [from + lead, to - tail]
}

/** Whether every line of the main selection already carries `mark` — the bar lights its button. */
export function hasMark(state: EditorState, mark: Mark): boolean {
  const doc = state.doc.toString()
  const range = state.selection.main
  if (range.empty) return closes(mark, runOf(doc, range.from, mark[0] as string, -1), runOf(doc, range.to, mark[0] as string, 1))
  const segs = segments(state, range)
  return segs.length > 0 && segs.every(([f, t]) => markAt(doc, mark, f, t) !== null)
}

/**
 * Toggle `mark` over each selection. A selection already wrapped — its markers just outside it,
 * or included at its edges — is unwrapped; anything else is wrapped, line by line, and stays
 * selected so a second mark can go on. With nothing selected the cursor either steps out of an
 * empty pair it is sitting in (`**|**`, which it then deletes) or gets a new pair to type into.
 */
export function toggleMark(mark: Mark): Plan {
  return (state) => {
    const doc = state.doc.toString()
    const n = mark.length
    return state.changeByRange((range) => {
      if (range.empty) {
        const c = mark[0] as string
        const inPair = runOf(doc, range.from, c, -1) >= n && runOf(doc, range.from, c, 1) >= n
        if (inPair && closes(mark, runOf(doc, range.from, c, -1), runOf(doc, range.from, c, 1))) {
          return {
            changes: { from: range.from - n, to: range.from + n },
            range: EditorSelection.cursor(range.from - n),
          }
        }
        return { changes: { from: range.from, insert: mark + mark }, range: EditorSelection.cursor(range.from + n) }
      }
      const segs = segments(state, range)
      if (segs.length === 0) return { range }
      const where = segs.map(([f, t]) => markAt(doc, mark, f, t))
      const unwrap = where.every((w) => w !== null)
      // Unwrapping takes the markers nearest the words, so `***x***` loses only the pair asked for.
      const changes = segs.flatMap(([f, t], i): ChangeSpec[] => {
        const on = where[i]
        if (unwrap && on) return [{ from: on[0] - n, to: on[0] }, { from: on[1], to: on[1] + n }]
        return on ? [] : [{ from: f, insert: mark }, { from: t, insert: mark }]
      })
      const set = state.changes(changes)
      // Keep the words selected, not the markers: mapped inward on both sides.
      const [first] = segs[0] as [number, number]
      const last = (segs[segs.length - 1] as [number, number])[1]
      return { changes: set, range: EditorSelection.range(set.mapPos(first, 1), set.mapPos(last, -1)) }
    })
  }
}

const URLISH = /^(?:[a-z][a-z0-9+.-]*:\/\/|mailto:|www\.)\S+$/iu

/**
 * Make the selection a link. Words become the text (`[words]()`, cursor between the
 * parentheses); a URL becomes the target (`[](url)`, cursor between the brackets); nothing
 * selected gives an empty `[]()` to fill in.
 */
export const insertLink: Plan = (state) =>
  state.changeByRange((range) => {
    const text = state.sliceDoc(range.from, range.to)
    if (text.includes('\n')) return { range }
    if (range.empty) return { changes: { from: range.from, insert: '[]()' }, range: EditorSelection.cursor(range.from + 1) }
    if (URLISH.test(text.trim())) {
      const insert = `[](${text.trim()})`
      return { changes: { from: range.from, to: range.to, insert }, range: EditorSelection.cursor(range.from + 1) }
    }
    const insert = `[${text}]()`
    return { changes: { from: range.from, to: range.to, insert }, range: EditorSelection.cursor(range.from + insert.length - 1) }
  })

/**
 * Link the selection to a note by name: `[[words]]`, or unwrap one that already is. With
 * nothing selected it opens an empty `[[]]` — the caller then starts completion inside it.
 */
export const toggleWikilink: Plan = (state) => {
  const doc = state.doc.toString()
  return state.changeByRange((range) => {
    const { from, to } = range
    if (doc.slice(from - 2, from) === '[[' && doc.slice(to, to + 2) === ']]' && doc[from - 3] !== '!') {
      return {
        changes: [{ from: from - 2, to: from }, { from: to, to: to + 2 }],
        range: EditorSelection.range(from - 2, to - 2),
      }
    }
    const text = doc.slice(from, to)
    if (text.includes('\n')) return { range }
    return {
      changes: [{ from, insert: '[[' }, { from: to, insert: ']]' }],
      range: range.empty ? EditorSelection.cursor(from + 2) : EditorSelection.range(from + 2, to + 2),
    }
  })
}

const TASK = /^(\s*)([-*+]|\d+[.)])(\s+)\[([ xX])\](\s|$)/u
const ITEM = /^(\s*)([-*+]|\d+[.)])(\s+)/u

/**
 * Checklist, line by line: a task item is ticked or unticked, a list item gains a box, and
 * any other line becomes an unticked task. Blank lines in a multi-line selection are left be.
 */
export const toggleTask: Plan = (state) => {
  const lines = new Set<number>()
  for (const r of state.selection.ranges) {
    for (let l = state.doc.lineAt(r.from).number; l <= state.doc.lineAt(r.to).number; l++) lines.add(l)
  }
  const many = lines.size > 1
  const changes: { from: number; to?: number; insert: string }[] = []
  for (const l of lines) {
    const line = state.doc.line(l)
    if (many && !line.text.trim()) continue
    const task = TASK.exec(line.text)
    if (task) {
      const box = line.from + (task[1] as string).length + (task[2] as string).length + (task[3] as string).length + 1
      changes.push({ from: box, to: box + 1, insert: task[4] === ' ' ? 'x' : ' ' })
      continue
    }
    const item = ITEM.exec(line.text)
    if (item) changes.push({ from: line.from + item[0].length, insert: '[ ] ' })
    else {
      const indent = /^\s*/u.exec(line.text)?.[0].length ?? 0
      changes.push({ from: line.from + indent, insert: '- [ ] ' })
    }
  }
  return changes.length ? { changes } : null
}
