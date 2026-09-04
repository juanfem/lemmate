import { test } from 'node:test'
import assert from 'node:assert/strict'
import { unnamedNote } from '../src/lib/notename.ts'

test('a note with no path is only "deleted" once there is a list it is missing from', () => {
  // The state a PWA comes back to after the phone kills it: tabs restored, vault doc still
  // on its way in from IndexedDB or the socket.
  assert.equal(unnamedNote({ noteOnly: false, vaultLoaded: false }), '')
  assert.equal(unnamedNote(undefined), '', 'no session yet either')
  assert.equal(unnamedNote({ noteOnly: false, vaultLoaded: true }), '(deleted)')
})

test('a directly shared note never has a path to look up', () => {
  assert.equal(unnamedNote({ noteOnly: true, vaultLoaded: false }), 'shared note')
  assert.equal(unnamedNote({ noteOnly: true, vaultLoaded: true }), 'shared note')
})
