import { test } from 'node:test'
import assert from 'node:assert/strict'
import { ancestors, basename, buildTree, countNotes, findFolder, folderOf, folderPaths, notesIn, rangeBetween, visibleNotes } from '../src/lib/tree.ts'

const notes = [
  { id: '1', path: 'inbox.md' },
  { id: '2', path: 'Daily/2026-08-31.md' },
  { id: '3', path: 'Daily/2026-08-30.md' },
  { id: '4', path: 'Daily/Weekly/w35.md' },
  { id: '5', path: 'Projects/lemmate/spec.md' },
]

test('folderOf and ancestors', () => {
  assert.equal(folderOf('inbox.md'), '')
  assert.equal(folderOf('Daily/Weekly/w35.md'), 'Daily/Weekly')
  assert.deepEqual(ancestors(''), [])
  assert.deepEqual(ancestors('a/b/c'), ['a', 'a/b', 'a/b/c'])
})

test('buildTree nests folders and sorts both levels', () => {
  const root = buildTree(notes)
  assert.deepEqual(
    root.folders.map((f) => f.name),
    ['Daily', 'Projects'],
  )
  assert.deepEqual(
    root.notes.map((n) => n.path),
    ['inbox.md'],
  )
  const daily = findFolder(root, 'Daily')!
  assert.deepEqual(
    daily.notes.map((n) => n.path),
    ['Daily/2026-08-30.md', 'Daily/2026-08-31.md'],
  )
  assert.equal(findFolder(root, 'Daily/Weekly')?.path, 'Daily/Weekly')
  assert.equal(findFolder(root, 'Nope/Here'), undefined)
  assert.equal(findFolder(root, ''), root)
})

test('countNotes and folderPaths reach the whole subtree', () => {
  const root = buildTree(notes)
  assert.equal(countNotes(root), 5)
  assert.equal(countNotes(findFolder(root, 'Daily')!), 3)
  assert.deepEqual(folderPaths(root), ['Daily', 'Daily/Weekly', 'Projects', 'Projects/lemmate'])
})

test('notesIn stops at the folder unless it is told to recurse', () => {
  const root = buildTree(notes)
  assert.deepEqual(
    notesIn(root, 'Daily', false).map((n) => n.id),
    ['3', '2'],
  )
  assert.deepEqual(
    notesIn(root, 'Daily', true).map((n) => n.id),
    ['3', '2', '4'],
  )
  assert.deepEqual(
    notesIn(root, '', false).map((n) => n.id),
    ['1'],
  )
  assert.equal(notesIn(root, '', true).length, 5)
  assert.deepEqual(notesIn(root, 'Missing', true), [])
})

test('a folder that only holds folders still shows up', () => {
  const root = buildTree([{ id: '1', path: 'a/b/c.md' }])
  assert.deepEqual(folderPaths(root), ['a', 'a/b'])
  assert.deepEqual(notesIn(root, 'a', false), [])
  assert.equal(notesIn(root, 'a', true).length, 1)
})

test('basename', () => {
  assert.equal(basename('a/b/c.md'), 'c.md')
  assert.equal(basename('c.md'), 'c.md')
  assert.equal(basename(''), '')
})

test('visibleNotes draws sub-folders before a folder\'s own notes, and skips folded ones', () => {
  const vaults = [
    { id: 'v1', label: 'One', notes },
    { id: 'v2', label: 'Two', notes: [{ id: '9', path: 'other.md' }] },
  ]
  assert.deepEqual(visibleNotes(vaults, {}), ['4', '3', '2', '5', '1', '9'])
  // Folding Daily hides its notes but not the folders after it.
  assert.deepEqual(visibleNotes(vaults, { 'v1/Daily': true }), ['5', '1', '9'])
  // Folding Daily/Weekly keeps Daily's own notes.
  assert.deepEqual(visibleNotes(vaults, { 'v1/Daily/Weekly': true }), ['3', '2', '5', '1', '9'])
  assert.deepEqual(visibleNotes(vaults, { v1: true }), ['9'])
})

test('rangeBetween works in either direction and survives a stale anchor', () => {
  const order = ['a', 'b', 'c', 'd']
  assert.deepEqual(rangeBetween(order, 'b', 'd'), ['b', 'c', 'd'])
  assert.deepEqual(rangeBetween(order, 'd', 'b'), ['b', 'c', 'd'])
  assert.deepEqual(rangeBetween(order, 'c', 'c'), ['c'])
  assert.deepEqual(rangeBetween(order, 'gone', 'c'), ['c'])
})
