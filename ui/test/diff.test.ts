// The version pane marks the lines an old version no longer shares with the note. The marks
// are the only thing standing between "this is the past" and "this is the past, and here is
// what moved", so the diff has to be right about *which* lines rather than merely how many.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { changedLines } from '../src/lib/diff.ts'

const changed = (a: string, b: string) => [...changedLines(a, b)].sort((x, y) => x - y)

test('nothing changed', () => {
  assert.deepEqual(changed('a\nb\nc', 'a\nb\nc'), [])
  assert.deepEqual(changed('', ''), [])
})

test('a line the note no longer has', () => {
  assert.deepEqual(changed('a\nb\nc', 'a\nc'), [1])
})

test('a line the note gained is not the old version to report', () => {
  assert.deepEqual(changed('a\nc', 'a\nb\nc'), [])
})

test('an edited line counts as gone', () => {
  assert.deepEqual(changed('a\nb\nc', 'a\nB\nc'), [1])
})

test('the whole thing rewritten', () => {
  assert.deepEqual(changed('a\nb', 'x\ny'), [0, 1])
})

test('a moved block is reported where it used to be', () => {
  // `b` survives only once: the LCS keeps the second copy, so the first is the one that went.
  assert.deepEqual(changed('b\na\nc', 'a\nb\nc'), [0])
})

test('shared head and tail are skipped, and the middle still lands right', () => {
  const head = Array.from({ length: 50 }, (_, i) => `h${i}`).join('\n')
  const tail = Array.from({ length: 50 }, (_, i) => `t${i}`).join('\n')
  assert.deepEqual(changed(`${head}\nmiddle\n${tail}`, `${head}\n${tail}`), [50])
})

test('an empty version against a full note', () => {
  assert.deepEqual(changed('', 'a\nb'), [])
  assert.deepEqual(changed('a\nb', ''), [0, 1])
})
