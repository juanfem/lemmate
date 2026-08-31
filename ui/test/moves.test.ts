import { test } from 'node:test'
import assert from 'node:assert/strict'
import { canDrop, isInside, movedFolderPath, movedPath, plan, restampId, uniquePath, type DragPayload } from '../src/lib/moves.ts'

const paths: Record<string, string> = {
  a: 'inbox.md',
  b: 'Daily/2026-08-31.md',
  c: 'Projects/lemmate/spec.md',
  d: 'Projects/lemmate/notes/crdt.md',
}
const at = (id: string) => paths[id]

test('movedPath keeps the file name and takes the new folder', () => {
  assert.equal(movedPath('Daily/2026-08-31.md', 'Archive'), 'Archive/2026-08-31.md')
  assert.equal(movedPath('Daily/2026-08-31.md', ''), '2026-08-31.md')
  assert.equal(movedPath('inbox.md', 'Reference/Books'), 'Reference/Books/inbox.md')
})

test('movedFolderPath carries the whole subtree', () => {
  assert.equal(movedFolderPath('Projects/lemmate/notes/crdt.md', 'Projects/lemmate', 'Archive'), 'Archive/lemmate/notes/crdt.md')
  assert.equal(movedFolderPath('Projects/lemmate/spec.md', 'Projects/lemmate', ''), 'lemmate/spec.md')
})

test('isInside', () => {
  assert.equal(isInside('a/b', 'a'), true)
  assert.equal(isInside('a', 'a'), false)
  assert.equal(isInside('ab/c', 'a'), false)
  assert.equal(isInside('anything', ''), true)
  assert.equal(isInside('', ''), false)
})

test('canDrop refuses a folder into itself, its own subtree, or where it already is', () => {
  const drag: DragPayload = { vault: 'v', notes: ['c', 'd'], folder: 'Projects/lemmate' }
  assert.equal(canDrop(drag, 'v', 'Projects/lemmate', at), false)
  assert.equal(canDrop(drag, 'v', 'Projects/lemmate/notes', at), false)
  assert.equal(canDrop(drag, 'v', 'Projects', at), false, 'already its parent')
  assert.equal(canDrop(drag, 'v', 'Archive', at), true)
  assert.equal(canDrop(drag, 'v', '', at), true)
  assert.equal(canDrop(drag, 'other', 'Projects/lemmate', at), true, 'another vault has its own tree')
})

test('canDrop refuses notes that are all already in the target folder', () => {
  const one: DragPayload = { vault: 'v', notes: ['b'] }
  assert.equal(canDrop(one, 'v', 'Daily', at), false)
  assert.equal(canDrop(one, 'v', 'Archive', at), true)
  assert.equal(canDrop({ vault: 'v', notes: ['a'] }, 'v', '', at), false, 'already at the root')
  // A mixed drag is worth offering: at least one note actually moves.
  assert.equal(canDrop({ vault: 'v', notes: ['a', 'b'] }, 'v', 'Daily', at), true)
})

test('plan lists only the notes that actually move', () => {
  assert.deepEqual(plan({ vault: 'v', notes: ['a', 'b'] }, 'Daily', at), [
    { id: 'a', from: 'inbox.md', to: 'Daily/inbox.md' },
  ])
  assert.deepEqual(plan({ vault: 'v', notes: ['c', 'd'], folder: 'Projects/lemmate' }, 'Archive', at), [
    { id: 'c', from: 'Projects/lemmate/spec.md', to: 'Archive/lemmate/spec.md' },
    { id: 'd', from: 'Projects/lemmate/notes/crdt.md', to: 'Archive/lemmate/notes/crdt.md' },
  ])
  assert.deepEqual(plan({ vault: 'v', notes: ['missing'] }, 'Daily', at), [])
})

test('uniquePath steps around names the target vault already uses', () => {
  const taken = new Set(['Ref/spec.md', 'Ref/spec 2.md', 'Ref/README'])
  assert.equal(uniquePath('Ref/other.md', taken), 'Ref/other.md')
  assert.equal(uniquePath('Ref/spec.md', taken), 'Ref/spec 3.md')
  assert.equal(uniquePath('Ref/README', taken), 'Ref/README 2')
  assert.equal(uniquePath('.gitignore', new Set(['.gitignore'])), '.gitignore 2', 'a dotfile has no stem to split')
})

test('restampId gives the copy its own id', () => {
  assert.equal(restampId('---\nid: OLD\ntitle: X\n---\nbody\n', 'NEW'), '---\nid: NEW\ntitle: X\n---\nbody\n')
  assert.equal(restampId('---\ntitle: X\n---\nbody\n', 'NEW'), '---\nid: NEW\ntitle: X\n---\nbody\n')
  assert.equal(restampId('# Just prose\n', 'NEW'), '---\nid: NEW\n---\n# Just prose\n')
  assert.equal(restampId('---\nunterminated\n', 'NEW'), '---\nid: NEW\n---\n---\nunterminated\n')
})
