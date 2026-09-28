// The version pane marks the lines an old version no longer shares with the note. The marks
// are the only thing standing between "this is the past" and "this is the past, and here is
// what moved", so the diff has to be right about *which* lines rather than merely how many.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { blocks, changedLines, inlineSpan, lineDiff } from '../src/lib/diff.ts'

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

// The changes view: both sides, in order, so a reader sees what went and what came.
const marks = (a: string, b: string) => lineDiff(a, b).map((l) => ({ same: ' ', add: '+', del: '-' })[l.kind] + l.text)

test('a unified diff keeps order and line numbers on both sides', () => {
  assert.deepEqual(marks('a\nb\nc', 'a\nB\nc\nd'), [' a', '-b', '+B', ' c', '+d'])
  const d = lineDiff('a\nb\nc', 'a\nB\nc\nd')
  assert.deepEqual(d.map((l) => [l.a, l.b]), [[0, 0], [1, undefined], [undefined, 1], [2, 2], [undefined, 3]])
  assert.deepEqual(marks('', 'x\ny'), ['+x', '+y'])
  assert.deepEqual(marks('x', ''), ['-x'])
})

test('a rewritten paragraph reads as the old one, then the new one', () => {
  assert.deepEqual(marks('h\none\ntwo\nt', 'h\nuno\ndos\nt'), [' h', '-one', '-two', '+uno', '+dos', ' t'])
})

test('past the table cap, the middle is replaced wholesale', () => {
  const a = Array.from({ length: 2100 }, (_, i) => `a${i}`).join('\n')
  const b = Array.from({ length: 2100 }, (_, i) => `b${i}`).join('\n')
  const d = lineDiff(`top\n${a}\nend`, `top\n${b}\nend`)
  assert.equal(d.filter((l) => l.kind === 'del').length, 2100)
  assert.equal(d[0]!.kind, 'same')
  assert.equal(d.at(-1)!.text, 'end')
})

test('blocks alternate between shared runs and changes', () => {
  const b = blocks(lineDiff('a\nb\nc\nd', 'a\nB\nc\nd\ne'))
  assert.deepEqual(
    b.map((x) => (x.type === 'same' ? `=${x.lines.length}` : `-${x.del.length}+${x.add.length}`)),
    ['=1', '-1+1', '=2', '-0+1'],
  )
})

test('a word changed in a long line is marked, whole words at a time', () => {
  const before = 'The quick brown fox jumps over the lazy dog'
  const after = 'The quick red fox jumps over the lazy dog'
  const [s, eb, ea] = inlineSpan(before, after)!
  assert.equal(before.slice(s, eb), 'brown')
  assert.equal(after.slice(s, ea), 'red')
  // Mid-word differences widen to the word.
  const [s2, eb2, ea2] = inlineSpan('call it colour here', 'call it color here')!
  assert.deepEqual(['call it colour here'.slice(s2, eb2), 'call it color here'.slice(s2, ea2)], ['colour', 'color'])
  // Nothing in common: the whole line is the change.
  assert.equal(inlineSpan('abc', 'xyz'), null)
  // An addition at the end.
  const [s3, eb3, ea3] = inlineSpan('one two', 'one two three')!
  assert.deepEqual(['one two'.slice(s3, eb3), 'one two three'.slice(s3, ea3)], ['', ' three'])
})
