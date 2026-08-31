// Preparing an Obsidian folder for upload (SPEC §11.4): the vault-relative names the multipart
// parts carry, and the batching that keeps each request under the server's body limit.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { batches, toUploads, totalBytes, type PickedFile } from '../src/lib/import.ts'

function file(webkitRelativePath: string, size = 10): PickedFile {
  return { name: webkitRelativePath.split('/').pop()!, size, webkitRelativePath }
}

test('the picked folder is the vault root, so its name comes off the paths', () => {
  const { uploads, root } = toUploads([
    file('MyVault/Daily/2026-01-01.md'),
    file('MyVault/attachments/logo.png'),
    file('MyVault/.obsidian/bookmarks.json'),
  ])
  assert.equal(root, 'MyVault')
  assert.deepEqual(
    uploads.map((u) => u.path),
    ['Daily/2026-01-01.md', 'attachments/logo.png', '.obsidian/bookmarks.json'],
  )
})

test('files from several roots keep their paths', () => {
  const { uploads, root } = toUploads([file('one/a.md'), file('two/b.md')])
  assert.equal(root, '')
  assert.deepEqual(
    uploads.map((u) => u.path),
    ['one/a.md', 'two/b.md'],
  )
})

test('a picker without relative paths falls back to bare names', () => {
  const { uploads, root } = toUploads([{ name: 'note.md', size: 3 }])
  assert.equal(root, '')
  assert.deepEqual(uploads[0]!.path, 'note.md')
})

test('batches respect the size and count limits, and never drop a file', () => {
  const { uploads } = toUploads([
    file('V/a.md', 6),
    file('V/b.md', 6),
    file('V/c.md', 1),
    file('V/big.png', 50),
    file('V/d.md', 1),
  ])
  const split = batches(uploads, { bytes: 10, count: 10 })
  assert.deepEqual(
    split.map((b) => b.map((u) => u.path)),
    [['a.md'], ['b.md', 'c.md'], ['big.png'], ['d.md']],
  )
  assert.equal(
    split.flat().length,
    uploads.length,
    'every picked file ends up in exactly one request',
  )
  // An oversized file gets a request of its own rather than being skipped here; whether it can
  // be stored at all is the server's call.
  assert.deepEqual(split[2]!.map((u) => u.path), ['big.png'])

  assert.deepEqual(
    batches(uploads, { bytes: 1000, count: 2 }).map((b) => b.length),
    [2, 2, 1],
  )
  assert.equal(totalBytes(uploads), 64)
})
