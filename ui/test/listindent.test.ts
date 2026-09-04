// Tab and Shift-Tab in a list: an item nests only when its marker reaches the column where the
// item above it holds its content, so the plan is about columns, not about indent units.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { parseItem, planIndent } from '../src/lib/editor/lists.ts'

/** Apply a plan to the whole document, the way the command's changes do. */
function apply(src: string, first: number, last: number, dir: 1 | -1): string {
  const lines = src.split('\n')
  for (const [i, text] of planIndent(lines, first, last, dir)) lines[i] = text
  return lines.join('\n')
}

test('an item is read as marker, content column and number', () => {
  assert.deepEqual(parseItem('- one'), { indent: 0, content: 2, number: null, prefix: 2 })
  assert.deepEqual(parseItem('  12. one'), { indent: 2, content: 6, number: 12, prefix: 6 })
  assert.equal(parseItem('    plain text'), null)
  assert.equal(parseItem('# not a list'), null)
  // A tab counts as up to four columns, as it does in markdown.
  assert.equal(parseItem('\t- one')?.indent, 4)
})

test('Tab nests under the item above, at its content column', () => {
  // Two spaces would not have done it: `1. ` holds its content at column three.
  assert.equal(apply('1. one\n2. two\n', 1, 1, 1), '1. one\n   1. two\n')
  assert.equal(apply('- one\n- two\n', 1, 1, 1), '- one\n  - two\n')
  // The item takes the number of its place in the list it lands in.
  assert.equal(apply('1. one\n   1. a\n2. two\n', 2, 2, 1), '1. one\n   1. a\n   2. two\n')
})

test('Shift-Tab comes back out to the parent, renumbered again', () => {
  assert.equal(apply('1. one\n   1. a\n   2. b\n', 2, 2, -1), '1. one\n   1. a\n2. b\n')
  assert.equal(apply('- one\n  - two\n', 1, 1, -1), '- one\n- two\n')
})

test('the first item of a list has nothing to nest under', () => {
  assert.equal(planIndent(['1. one', '2. two'], 0, 0, 1).size, 0)
  assert.equal(planIndent(['- one'], 0, 0, 1).size, 0)
  // And a top-level item has nowhere to come out to.
  assert.equal(planIndent(['- one', '- two'], 1, 1, -1).size, 0)
  // Neither does a line that is not a list item at all — Tab then means what it always meant.
  assert.equal(planIndent(['just prose'], 0, 0, 1).size, 0)
})

test('an item takes its children with it', () => {
  const src = '- one\n- two\n  - child\n    - grandchild\n- three\n'
  assert.equal(apply(src, 1, 1, 1), '- one\n  - two\n    - child\n      - grandchild\n- three\n')
  // Continuation text under the item moves too, keeping its own relative indent.
  assert.equal(apply('- one\n- two\n  more\n', 1, 1, 1), '- one\n  - two\n    more\n')
})

test('a selection moves every item in it, once', () => {
  const src = '- one\n- two\n- three\n'
  // The second and third both nest under the first; the third is not moved twice for being a
  // child of the second after the first move — the plan is made against the original text.
  assert.equal(apply(src, 1, 2, 1), '- one\n  - two\n  - three\n')
})

test('an item with a child in the selection is not moved twice', () => {
  const src = '- one\n- two\n  - child\n'
  assert.equal(apply(src, 1, 2, 1), '- one\n  - two\n    - child\n')
})

test('a blank line inside a loose list does not end the item', () => {
  assert.equal(apply('- one\n- two\n\n  more\n', 1, 1, 1), '- one\n  - two\n\n    more\n')
})
