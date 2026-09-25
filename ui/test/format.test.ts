// Formatting commands: the bar, the context menu and Mod-B/I share them, so what they do to the
// text is pinned here. `|` marks the cursor; `«` and `»` the selection.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { EditorSelection, EditorState } from '@codemirror/state'
import { hasMark, insertLink, toggleMark, toggleTask, toggleWikilink, type Plan } from '../src/lib/editor/format.ts'

function state(marked: string): EditorState {
  const cursor = marked.indexOf('|')
  if (cursor >= 0) return EditorState.create({ doc: marked.replace('|', ''), selection: { anchor: cursor } })
  const from = marked.indexOf('«')
  const to = marked.indexOf('»') - 1
  return EditorState.create({
    doc: marked.slice(0, from) + marked.slice(from + 1, to + 1) + marked.slice(to + 2),
    selection: EditorSelection.single(from, to),
  })
}

function show(s: EditorState): string {
  const { from, to, empty } = s.selection.main
  const doc = s.doc.toString()
  return empty ? `${doc.slice(0, from)}|${doc.slice(from)}` : `${doc.slice(0, from)}«${doc.slice(from, to)}»${doc.slice(to)}`
}

function apply(plan: Plan, marked: string): string {
  const s = state(marked)
  const spec = plan(s)
  return spec ? show(s.update(spec).state) : show(s)
}

test('a selection is wrapped, and stays selected without its markers', () => {
  assert.equal(apply(toggleMark('**'), 'a «word» b'), 'a **«word»** b')
  assert.equal(apply(toggleMark('~~'), '«gone»'), '~~«gone»~~')
  assert.equal(apply(toggleMark('`'), 'run «ls» now'), 'run `«ls»` now')
})

test('toggling again unwraps, whether the markers are outside the selection or inside it', () => {
  assert.equal(apply(toggleMark('**'), 'a **«word»** b'), 'a «word» b')
  assert.equal(apply(toggleMark('**'), 'a «**word**» b'), 'a «word» b')
  assert.equal(apply(toggleMark('`'), '`«ls»`'), '«ls»')
})

test('italic and bold tell their asterisks apart', () => {
  // Italic over bold text adds a star rather than stripping one of the bold pair…
  assert.equal(apply(toggleMark('*'), '**«x»**'), '***«x»***')
  // …and each comes off a bold-italic run on its own.
  assert.equal(apply(toggleMark('*'), '***«x»***'), '**«x»**')
  assert.equal(apply(toggleMark('**'), '***«x»***'), '*«x»*')
  assert.equal(apply(toggleMark('**'), '*«x»*'), '***«x»***')
  assert.equal(hasMark(state('**«x»**'), '*'), false)
  assert.equal(hasMark(state('**«x»**'), '**'), true)
  assert.equal(hasMark(state('***«x»***'), '*'), true)
})

test('the spaces a double-click selects stay outside the markers', () => {
  // `**word **` is not bold in markdown: the closing run may not follow a space.
  assert.equal(apply(toggleMark('**'), 'a «word »b'), 'a **«word»** b')
})

test('a selection across lines is wrapped line by line', () => {
  assert.equal(apply(toggleMark('*'), '«one\n\ntwo»'), '*«one*\n\n*two»*')
  assert.equal(apply(toggleMark('*'), '*«one*\n\n*two»*'), '«one\n\ntwo»')
})

test('with nothing selected, a pair opens around the cursor and closes again if left empty', () => {
  assert.equal(apply(toggleMark('**'), 'a |b'), 'a **|**b')
  assert.equal(apply(toggleMark('**'), 'a **|**b'), 'a |b')
})

test('a link takes words as its text and a URL as its target', () => {
  assert.equal(apply(insertLink, 'see «the docs» here'), 'see [the docs](|) here')
  assert.equal(apply(insertLink, '«https://example.org/x»'), '[|](https://example.org/x)')
  assert.equal(apply(insertLink, 'x |'), 'x [|]()')
})

test('a note link wraps the selection and comes off again', () => {
  assert.equal(apply(toggleWikilink, 'see «Plan» now'), 'see [[«Plan»]] now')
  assert.equal(apply(toggleWikilink, 'see [[«Plan»]] now'), 'see «Plan» now')
  assert.equal(apply(toggleWikilink, 'x |'), 'x [[|]]')
})

test('a checklist box goes on any line, and ticks when it is already there', () => {
  assert.equal(apply(toggleTask, 'buy milk|'), '- [ ] buy milk|')
  assert.equal(apply(toggleTask, '  - buy milk|'), '  - [ ] buy milk|')
  assert.equal(apply(toggleTask, '1. buy milk|'), '1. [ ] buy milk|')
  assert.equal(apply(toggleTask, '- [ ] buy milk|'), '- [x] buy milk|')
  assert.equal(apply(toggleTask, '- [x] buy milk|'), '- [ ] buy milk|')
})

test('a checklist over several lines leaves the blank ones alone', () => {
  const s = EditorState.create({ doc: 'one\n\ntwo', selection: EditorSelection.single(0, 8) })
  const spec = toggleTask(s)
  assert.ok(spec)
  assert.equal(s.update(spec).state.doc.toString(), '- [ ] one\n\n- [ ] two')
})
