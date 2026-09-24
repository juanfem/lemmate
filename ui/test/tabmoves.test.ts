import { test } from 'node:test'
import assert from 'node:assert/strict'
import { clampIndex, endedOutside, inNewPane, moveTab, notePane, removeTab, type TabPane } from '../src/lib/tabmoves.ts'
import { renderTab, tabNote } from '../src/lib/rendertabs.ts'

let seq = 100
const fresh = (tab: string): TabPane => ({ id: ++seq, tabs: [tab], active: tab })
const pane = (id: number, tabs: string[], active: string | null = tabs[0] ?? null, kind?: 'history'): TabPane => ({ id, tabs, active, kind })
const shape = (r: { panes: TabPane[]; focused: number } | null) =>
  r && { panes: r.panes.map((p) => `${p.tabs.map((t) => (t === p.active ? `[${t}]` : t)).join(' ')}`), focused: r.focused }

test('reordering within a strip', () => {
  const panes = [pane(1, ['a', 'b', 'c'])]
  // Positions count the strip without the dragged tab: a between b and c is index 1.
  assert.deepEqual(shape(moveTab(panes, { tab: 'a', pane: 1 }, { pane: 1, index: 1 }, [], 3, fresh)), { panes: ['b [a] c'], focused: 0 })
  assert.deepEqual(shape(moveTab(panes, { tab: 'c', pane: 1 }, { pane: 1, index: 0 }, [], 3, fresh)), { panes: ['[c] a b'], focused: 0 })
  assert.deepEqual(shape(moveTab(panes, { tab: 'a', pane: 1 }, { pane: 1, index: 99 }, [], 3, fresh)), { panes: ['b c [a]'], focused: 0 })
})

test('dropping a tab where it already is changes nothing', () => {
  const panes = [pane(1, ['a', 'b'], 'b')]
  assert.equal(moveTab(panes, { tab: 'b', pane: 1 }, { pane: 1, index: 1 }, [], 3, fresh), null)
  // …but it does make an inactive tab the active one.
  assert.deepEqual(shape(moveTab(panes, { tab: 'a', pane: 1 }, { pane: 1, index: 0 }, [], 3, fresh)), { panes: ['[a] b'], focused: 0 })
})

test('pinned and unpinned tabs stay in their own groups', () => {
  const pinned = ['p', 'q']
  assert.equal(clampIndex(['p', 'q', 'a', 'b'], 'x', 0, pinned), 2)
  assert.equal(clampIndex(['q', 'a', 'b'], 'p', 3, pinned), 1)
  const panes = [pane(1, ['p', 'q', 'a', 'b'])]
  assert.deepEqual(shape(moveTab(panes, { tab: 'b', pane: 1 }, { pane: 1, index: 0 }, pinned, 3, fresh))?.panes, ['p q [b] a'])
  assert.deepEqual(shape(moveTab(panes, { tab: 'p', pane: 1 }, { pane: 1, index: 3 }, pinned, 3, fresh))?.panes, ['q [p] a b'])
})

test('into another pane: the source picks its neighbour, an emptied pane goes', () => {
  const panes = [pane(1, ['a', 'b', 'c'], 'b'), pane(2, ['x'])]
  assert.deepEqual(shape(moveTab(panes, { tab: 'b', pane: 1 }, { pane: 2, index: 1 }, [], 3, fresh)), { panes: ['a [c]', 'x [b]'], focused: 1 })
  const two = [pane(1, ['a']), pane(2, ['x'])]
  assert.deepEqual(shape(moveTab(two, { tab: 'a', pane: 1 }, { pane: 2, index: 0 }, [], 3, fresh)), { panes: ['[a] x'], focused: 0 })
})

test('a note already open in the target is not doubled', () => {
  const panes = [pane(1, ['a', 'b']), pane(2, ['x', 'a'], 'x')]
  assert.deepEqual(shape(moveTab(panes, { tab: 'a', pane: 1 }, { pane: 2, index: 0 }, [], 3, fresh))?.panes, ['[b]', '[a] x'])
})

test('splitting to either side', () => {
  const panes = [pane(1, ['a', 'b']), pane(2, ['x'])]
  assert.deepEqual(shape(moveTab(panes, { tab: 'b', pane: 1 }, { pane: 2, split: 'right' }, [], 3, fresh)), { panes: ['[a]', '[x]', '[b]'], focused: 2 })
  assert.deepEqual(shape(moveTab(panes, { tab: 'b', pane: 1 }, { pane: 1, split: 'left' }, [], 3, fresh)), { panes: ['[b]', '[a]', '[x]'], focused: 0 })
  // Split off a pane's only tab into a split of another pane: the source goes.
  assert.deepEqual(shape(moveTab(panes, { tab: 'x', pane: 2 }, { pane: 1, split: 'left' }, [], 3, fresh)), { panes: ['[x]', '[a] b'], focused: 0 })
})

test('splits that cannot happen', () => {
  // A pane's only tab cannot split that same pane.
  assert.equal(moveTab([pane(1, ['a']), pane(2, ['x'])], { tab: 'a', pane: 1 }, { pane: 1, split: 'right' }, [], 3, fresh), null)
  // At the limit a split lands in the target pane instead.
  const full = [pane(1, ['a', 'b']), pane(2, ['x']), pane(3, ['y'])]
  assert.deepEqual(shape(moveTab(full, { tab: 'b', pane: 1 }, { pane: 3, split: 'right' }, [], 3, fresh))?.panes, ['[a]', '[x]', 'y [b]'])
})

test('history panes are neither sources nor targets', () => {
  const panes = [pane(1, ['a', 'b']), pane(2, ['a'], 'a', 'history')]
  assert.equal(moveTab(panes, { tab: 'b', pane: 1 }, { pane: 2, index: 0 }, [], 3, fresh), null)
  assert.equal(moveTab(panes, { tab: 'a', pane: 2 }, { pane: 1, index: 0 }, [], 3, fresh), null)
})

test('a tab from another window arrives without leaving anything here', () => {
  const panes = [pane(1, ['a', 'b']), pane(2, ['x'])]
  assert.deepEqual(shape(moveTab(panes, { tab: 'n', pane: null }, { pane: 2, index: 0 }, [], 3, fresh)), { panes: ['[a] b', '[n] x'], focused: 1 })
  assert.deepEqual(shape(moveTab(panes, { tab: 'n', pane: null }, { pane: 1, split: 'left' }, [], 3, fresh)), { panes: ['[n]', '[a] b', '[x]'], focused: 0 })
  // Already open in the target pane: it is not doubled, just moved to where it was dropped.
  assert.deepEqual(shape(moveTab(panes, { tab: 'b', pane: null }, { pane: 1, index: 0 }, [], 3, fresh))?.panes, ['[b] a', '[x]'])
  assert.equal(moveTab([pane(1, ['a'], 'a', 'history')], { tab: 'n', pane: null }, { pane: 1, index: 0 }, [], 3, fresh), null)
})

test('removeTab: the window a tab was dragged out of', () => {
  const panes = [pane(1, ['a', 'b', 'c'], 'b'), pane(2, ['x'])]
  assert.deepEqual(removeTab(panes, { tab: 'b', pane: 1 }).map((p) => `${p.tabs.join(' ')}|${p.active}`), ['a c|c', 'x|x'])
  assert.deepEqual(removeTab(panes, { tab: 'x', pane: 2 }).map((p) => p.tabs.join(' ')), ['a b c'])
  // The last pane stays, empty: a detached window closes itself on that.
  assert.deepEqual(removeTab([pane(1, ['a'])], { tab: 'a', pane: 1 }).map((p) => `${p.tabs.length}|${p.active}`), ['0|null'])
  // Nothing to take away.
  assert.equal(removeTab(panes, { tab: 'zz', pane: 1 }), panes)
})

test('endedOutside: where a drag nobody took was let go', () => {
  const at = (clientX: number, clientY: number, screenX = 500, screenY = 400) => endedOutside({ clientX, clientY, screenX, screenY }, 1000, 800)
  assert.equal(at(500, 400), false)
  assert.equal(at(0, 0), false)
  assert.equal(at(-1, 400), true)
  assert.equal(at(500, -30), true)
  assert.equal(at(1000, 400), true)
  assert.equal(at(500, 800), true)
  // No position at all (an engine that does not report one): not outside, so no window opens.
  assert.equal(at(-200, -200, 0, 0), false)
})

test('a rendered tab moves like any other: into a pane of notes, out of it, to another window', () => {
  const ra = renderTab('a')
  const notes = pane(1, ['a', 'b'])
  const renders = pane(2, [ra, renderTab('c')], ra)
  // Into the notes, beside the note it renders.
  assert.deepEqual(shape(moveTab([notes, renders], { tab: ra, pane: 2 }, { pane: 1, index: 1 }, [], 3, fresh)), {
    panes: [`a [${ra}] b`, `[${renderTab('c')}]`],
    focused: 0,
  })
  // A note into the renders.
  assert.deepEqual(moveTab([notes, renders], { tab: 'b', pane: 1 }, { pane: 2, index: 0 }, [], 3, fresh)!.panes[1]!.tabs, ['b', ra, renderTab('c')])
  // From another window, and split off at an edge.
  assert.deepEqual(moveTab([notes], { tab: ra, pane: null }, { pane: 1, index: 9 }, [], 3, fresh)!.panes[0]!.tabs, ['a', 'b', ra])
  assert.deepEqual(shape(moveTab([notes, renders], { tab: ra, pane: 2 }, { pane: 1, split: 'left' }, [], 3, fresh)), {
    panes: [`[${ra}]`, '[a] b', `[${renderTab('c')}]`],
    focused: 0,
  })
  // Still never into or out of a history pane.
  const history = pane(3, ['a'], 'a', 'history')
  assert.equal(moveTab([renders, history], { tab: ra, pane: 2 }, { pane: 3, index: 0 }, [], 3, fresh), null)
  assert.equal(tabNote(ra), 'a')
  assert.equal(tabNote('a'), 'a')
})

test('a note opened while a render or history is in front never covers it', () => {
  const blank = (): TabPane => ({ id: 99, tabs: [], active: null })
  const note = pane(1, ['a'])
  const render = pane(2, [renderTab('a')])
  const history = pane(3, ['b'], 'b', 'history')
  // A pane with a note in front is there: the focus goes to it.
  assert.deepEqual(notePane([note, render], 1, 3, blank), { panes: [note, render], focused: 0 })
  assert.deepEqual(notePane([note, history], 1, 3, blank), { panes: [note, history], focused: 0 })
  // Already on one: nothing moves.
  assert.deepEqual(notePane([note, render], 0, 3, blank), { panes: [note, render], focused: 0 })
  // Only renders open: the focused one stays, and the note becomes a tab beside them.
  assert.deepEqual(notePane([render], 0, 3, blank), { panes: [render], focused: 0 })
  // From a history pane, any pane of tabs will do.
  assert.deepEqual(notePane([render, history], 1, 3, blank), { panes: [render, history], focused: 0 })
  // The last one was closed: a new one, to the left.
  const made = notePane([history], 0, 3, blank)
  assert.equal(made.focused, 0)
  assert.deepEqual(made.panes.map((p) => p.kind ?? 'note'), ['note', 'history'])
  // No room for one: the focused history pane gives up its place.
  const h = (id: number) => pane(id, ['x'], 'x', 'history')
  const full = notePane([h(4), history, h(5)], 1, 3, blank)
  assert.equal(full.focused, 1)
  assert.deepEqual(full.panes.map((p) => p.kind ?? 'note'), ['history', 'note', 'history'])
})

test('opening in a new pane never displaces what is open', () => {
  const fresh2 = (tab: string): TabPane => ({ id: ++seq, tabs: [tab], active: tab })
  // Room: a pane of its own, right of the focused one, and the focus goes with it.
  assert.deepEqual(shape(inNewPane([pane(1, ['a'])], 0, 3, 'b', fresh2)), { panes: ['[a]', '[b]'], focused: 1 })
  assert.deepEqual(shape(inNewPane([pane(1, ['a']), pane(2, ['c'])], 0, 3, 'b', fresh2)), { panes: ['[a]', '[b]', '[c]'], focused: 1 })
  // Full: a tab of its own in the next pane of notes; the note it lands beside stays open.
  const full = [pane(1, ['a']), pane(2, ['c']), pane(3, ['d'])]
  assert.deepEqual(shape(inNewPane(full, 0, 3, 'b', fresh2)), { panes: ['[a]', 'c [b]', '[d]'], focused: 1 })
  assert.deepEqual(shape(inNewPane(full, 2, 3, 'b', fresh2)), { panes: ['a [b]', '[c]', '[d]'], focused: 0 })
  // Already open there: focused, not doubled.
  assert.deepEqual(shape(inNewPane([pane(1, ['a']), pane(2, ['b', 'c'], 'c'), pane(3, ['d'])], 0, 3, 'b', fresh2)), { panes: ['[a]', '[b] c', '[d]'], focused: 1 })
  // History panes are skipped: a note does not go into one.
  const asides = [pane(1, ['a']), pane(2, ['a'], 'a', 'history'), pane(3, ['c'], 'c', 'history')]
  assert.deepEqual(shape(inNewPane(asides, 0, 3, 'b', fresh2)), { panes: ['a [b]', '[a]', '[c]'], focused: 0 })
})
