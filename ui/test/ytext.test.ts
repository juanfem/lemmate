// A machine edit to a note — a renamed link, a tag taken off — replaces only what differs.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import * as Y from 'yjs'
import { replaceText } from '../src/lib/ytext.ts'

test('only the differing span is replaced', () => {
  const doc = new Y.Doc()
  const text = doc.getText('content')
  text.insert(0, 'see [[Old]] and [[Old|alias]]\n')
  const deltas: unknown[] = []
  text.observe((e) => deltas.push(e.delta))
  assert.equal(replaceText(text, 'see [[New]] and [[New|alias]]\n'), true)
  assert.equal(text.toString(), 'see [[New]] and [[New|alias]]\n')
  // One transaction, starting at the first difference rather than at 0.
  assert.equal(deltas.length, 1)
  assert.deepEqual((deltas[0] as { retain?: number }[])[0], { retain: 6 })
  assert.equal(replaceText(text, text.toString()), false)
})

test('an edit made concurrently elsewhere in the note survives', () => {
  const a = new Y.Doc()
  a.getText('content').insert(0, 'intro\n[[Old]]\noutro\n')
  const b = new Y.Doc()
  Y.applyUpdate(b, Y.encodeStateAsUpdate(a))
  replaceText(a.getText('content'), 'intro\n[[New]]\noutro\n')
  b.getText('content').insert(0, 'typed meanwhile ')
  Y.applyUpdate(a, Y.encodeStateAsUpdate(b))
  Y.applyUpdate(b, Y.encodeStateAsUpdate(a))
  assert.equal(a.getText('content').toString(), 'typed meanwhile intro\n[[New]]\noutro\n')
  assert.equal(b.getText('content').toString(), a.getText('content').toString())
})

test('a surrogate pair is never cut in half', () => {
  const doc = new Y.Doc()
  const text = doc.getText('content')
  text.insert(0, 'a😀b')
  replaceText(text, 'a😃b') // same high surrogate, different low one
  assert.equal(text.toString(), 'a😃b')
  replaceText(text, 'a😃😀b')
  assert.equal(text.toString(), 'a😃😀b')
  replaceText(text, 'ab')
  assert.equal(text.toString(), 'ab')
})
