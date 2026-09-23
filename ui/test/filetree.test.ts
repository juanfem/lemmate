// The attachments view's tree: folders that hold files, folded chains, folders before files.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { buildFileTree, fileKind, foldersToReveal, freeName, humanSize, isText, type FileNode } from '../src/lib/filetree.ts'
import type { FileEntry } from '../src/lib/api.ts'

const entry = (path: string): FileEntry => ({ path, hash: 'h', size: 1, used_by: [], kept: false, vault: false })

/** The tree as indented lines, `/` after a folder and its count. */
function draw(nodes: FileNode[], depth = 0): string[] {
  return nodes.flatMap((n) =>
    n.kind === 'folder'
      ? [`${'  '.repeat(depth)}${n.label}/ ${n.count}`, ...draw(n.children, depth + 1)]
      : [`${'  '.repeat(depth)}${n.name}`],
  )
}

test('folders first, then files; chains of lone folders fold into one row', () => {
  const tree = buildFileTree(
    [
      '_quarto.yml',
      'Slides/2026-09 Review/custom.scss',
      'Slides/2026-09 Review/_vars.scss',
      'Slides/2026-09 Review/img/pipeline.png',
      'Slides/2026-06 Kickoff/theme.scss',
      'Archive/2024/Old decks/a.png',
      'Archive/2024/Old decks/b.png',
      'attachments/kettle.jpg',
      'attachments/Whiteboard 10.png',
      'attachments/Whiteboard 9.png',
    ].map(entry),
  )
  assert.deepEqual(draw(tree), [
    'Archive / 2024 / Old decks/ 2',
    '  a.png',
    '  b.png',
    'attachments/ 3',
    '  kettle.jpg',
    '  Whiteboard 9.png',
    '  Whiteboard 10.png',
    'Slides/ 4',
    '  2026-06 Kickoff/ 1',
    '    theme.scss',
    '  2026-09 Review/ 3',
    '    img/ 1',
    '      pipeline.png',
    '    _vars.scss',
    '    custom.scss',
    '_quarto.yml',
  ])
  const archive = tree[0]!
  assert.equal(archive.kind === 'folder' && archive.path, 'Archive/2024/Old decks')
})

test('a filter leaves out files, and the folders left with none', () => {
  const tree = buildFileTree(['a/x.png', 'b/y.scss', 'c.png'].map(entry), (f) => fileKind(f.path) === 'image')
  assert.deepEqual(draw(tree), ['a/ 1', '  x.png', 'c.png'])
})

test('kinds, and what opens as text', () => {
  assert.equal(fileKind('a/B.PNG'), 'image')
  assert.equal(fileKind('custom.scss'), 'style')
  assert.equal(fileKind('_quarto.yml'), 'style')
  assert.equal(fileKind('report.pdf'), 'other')
  assert.ok(isText('custom.scss') && isText('notes.txt') && isText('logo.svg'))
  assert.ok(!isText('pic.png') && !isText('report.pdf') && !isText('Makefile'))
})

test('the folders to open to show a file', () => {
  assert.deepEqual(foldersToReveal(['a/b/c.png', 'a/d.png', 'e.png']).sort(), ['a', 'a/b'])
})

test('a free name beside a taken one', () => {
  const taken = new Set(['talks/custom.scss', 'talks/custom-2.scss', 'README'])
  const has = (p: string) => taken.has(p)
  assert.equal(freeName(has, 'talks', 'new.scss'), 'talks/new.scss')
  assert.equal(freeName(has, 'talks', 'custom.scss'), 'talks/custom-3.scss')
  assert.equal(freeName(has, '', 'README'), 'README-2')
})

test('sizes', () => {
  assert.equal(humanSize(312), '312 B')
  assert.equal(humanSize(1229), '1.2 KB')
  assert.equal(humanSize(4.6 * 1024 * 1024), '4.6 MB')
  assert.equal(humanSize(null), '')
})
