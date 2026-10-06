import { test } from 'node:test'
import assert from 'node:assert/strict'
import { tabKey } from '../src/lib/tabkeys.ts'

const key = (key: string, code: string, mods: { ctrl?: boolean; shift?: boolean; alt?: boolean; meta?: boolean } = {}) => ({
  key,
  code,
  ctrlKey: !!mods.ctrl,
  shiftKey: !!mods.shift,
  altKey: !!mods.alt,
  metaKey: !!mods.meta,
})

test('Ctrl+Tab and Ctrl+Shift+Tab step along the tabs', () => {
  assert.deepEqual(tabKey(key('Tab', 'Tab', { ctrl: true })), { step: 1 })
  assert.deepEqual(tabKey(key('Tab', 'Tab', { ctrl: true, shift: true })), { step: -1 })
  assert.equal(tabKey(key('Tab', 'Tab')), null)
  assert.equal(tabKey(key('Tab', 'Tab', { shift: true })), null)
})

test('Ctrl+Shift+[ / ] step too, by code whatever the shifted key reads', () => {
  assert.deepEqual(tabKey(key('}', 'BracketRight', { ctrl: true, shift: true })), { step: 1 })
  assert.deepEqual(tabKey(key('{', 'BracketLeft', { ctrl: true, shift: true })), { step: -1 })
  // Without Shift they are the editor's indent; with Alt instead, its fold-all and daily stepping.
  assert.equal(tabKey(key('[', 'BracketLeft', { ctrl: true })), null)
  assert.equal(tabKey(key('[', 'BracketLeft', { ctrl: true, alt: true })), null)
  assert.equal(tabKey(key('[', 'BracketLeft', { alt: true })), null)
})

test('Cmd stays the browser’s: Cmd+Shift+[ / ] is how Chrome on a Mac switches its tabs', () => {
  assert.equal(tabKey(key('}', 'BracketRight', { meta: true, shift: true })), null)
  assert.equal(tabKey(key('Tab', 'Tab', { ctrl: true, meta: true })), null)
})

test('Alt+W closes, Option+W included', () => {
  assert.equal(tabKey(key('w', 'KeyW', { alt: true })), 'close')
  assert.equal(tabKey(key('∑', 'KeyW', { alt: true })), 'close')
  assert.equal(tabKey(key('W', 'KeyW', { alt: true, shift: true })), null)
  assert.equal(tabKey(key('w', 'KeyW', { alt: true, ctrl: true })), null)
  assert.equal(tabKey(key('w', 'KeyW')), null)
})
