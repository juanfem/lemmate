import { test } from 'node:test'
import assert from 'node:assert/strict'
import { searchNotes, snippet, terms } from '../src/lib/search.ts'

const notes = [
  { id: 'a', vault: 'v', title: 'Invoices', text: 'How I file invoices each quarter.' },
  { id: 'b', vault: 'v', title: 'Daily 2026-09-01', text: 'Paid the invoice and filed it away.' },
  { id: 'c', vault: 'v', title: 'Recipes', text: 'Nothing to do with money at all.' },
]

test('terms splits and lower-cases, ignoring the gaps', () => {
  assert.deepEqual(terms('  Foo   BAR '), ['foo', 'bar'])
  assert.deepEqual(terms('   '), [])
})

test('an empty query matches nothing rather than everything', () => {
  assert.deepEqual(searchNotes(notes, ''), [])
  assert.deepEqual(searchNotes(notes, '   '), [])
})

test('every term must appear somewhere in the note', () => {
  assert.deepEqual(searchNotes(notes, 'invoice').map((h) => h.note_id), ['a', 'b'])
  // "quarter" is only in a; both terms together therefore only match a.
  assert.deepEqual(searchNotes(notes, 'invoice quarter').map((h) => h.note_id), ['a'])
  assert.deepEqual(searchNotes(notes, 'invoice unicorn'), [])
})

test('a title hit outranks a body hit', () => {
  // b mentions "invoice" in its body; a is called Invoices. a comes first.
  assert.equal(searchNotes(notes, 'invoice')[0]?.note_id, 'a')
})

test('the limit is honoured', () => {
  assert.equal(searchNotes(notes, 'invoice', 1).length, 1)
})

test('snippets bracket the match, the way the server does', () => {
  assert.equal(snippet('Paid the invoice and filed it away.', ['invoice']), 'Paid the [invoice] and filed it away.')
})

test('a long note is trimmed to a window with ellipses', () => {
  const text = `${'pad '.repeat(40)}needle ${'tail '.repeat(40)}`.trim()
  const s = snippet(text, ['needle'])
  assert.ok(s.startsWith('…'), s)
  assert.ok(s.endsWith('…'), s)
  assert.ok(s.includes('[needle]'), s)
  assert.ok(s.split(/\s+/u).length <= 15, `window should stay small, got ${s.split(/\s+/u).length}`)
})

test('a note with no match still yields a readable opening', () => {
  assert.equal(snippet('one two three', ['zzz']), 'one two three')
})

test('matching is by substring, unlike the server, and that is on purpose', () => {
  // SQLite FTS5 tokenises: searching "invoice" on the server does not find "invoices".
  // Offline it does. Pinned here so the divergence stays a decision rather than a surprise.
  const only = [{ id: 'a', vault: 'v', title: 'Invoices', text: 'filed quarterly' }]
  assert.equal(searchNotes(only, 'invoice').length, 1)
  assert.equal(searchNotes(only, 'quarter').length, 1, 'a prefix of a body word matches too')
  assert.equal(searchNotes(only, 'invoicing').length, 0, 'but it is a substring test, not a stemmer')
})
