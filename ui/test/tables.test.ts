// Live preview draws a GFM table as a table: cells by column (the parser drops empty ones),
// alignment from the delimiter row, and the inline markup inside each cell.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { GFM, parser } from '@lezer/markdown'
import type { SyntaxNode } from '@lezer/common'
import { noteSyntax } from '../src/lib/editor/syntax.ts'
import { tableModel, type Inline, type TableCell } from '../src/lib/editor/livePreview.ts'

const p = parser.configure([GFM, noteSyntax])

function model(src: string) {
  let table: SyntaxNode | null = null
  p.parse(src).iterate({
    enter: (n) => {
      if (n.name === 'Table' && !table) table = n.node
    },
  })
  assert.ok(table, 'no table parsed')
  return tableModel((f, t) => src.slice(f, t), table)
}

const plain = (content: Inline[]): string =>
  content.map((i) => ('text' in i ? i.text : 'children' in i ? plain(i.children) : 'label' in i ? i.label : i.tex)).join('')
const texts = (cells: (TableCell | null)[]) => cells.map((c) => (c ? plain(c.content) : null))

test('cells land in their columns, empty ones included', () => {
  const m = model('| a | b | c |\n|---|:-:|--:|\n| 1 |   | 3 |\n| | 2 |\nx | y | z | extra\n')
  assert.deepEqual(m.align, [null, 'center', 'right'])
  assert.deepEqual(texts(m.header), ['a', 'b', 'c'])
  assert.deepEqual(m.rows.map(texts), [
    ['1', null, '3'],
    [null, '2', null],
    ['x', 'y', 'z'],
  ])
})

test('a table without outer pipes', () => {
  const m = model('h1 | h2\n:-- | ---\nv1 | v2\n')
  assert.deepEqual(m.align, ['left', null])
  assert.deepEqual(texts(m.header), ['h1', 'h2'])
  assert.deepEqual(m.rows.map(texts), [['v1', 'v2']])
})

test('inline markup inside cells', () => {
  const src = '| x |\n|---|\n| **b** _i_ `c` [t](http://e \"ti\") [[Note\\|lbl]] #tag $x^2$ a\\|b |\n'
  const cell = model(src).rows[0]![0]!
  assert.deepEqual(cell.content, [
    { kind: 'strong', children: [{ kind: 'text', text: 'b' }] },
    { kind: 'text', text: ' ' },
    { kind: 'em', children: [{ kind: 'text', text: 'i' }] },
    { kind: 'text', text: ' ' },
    { kind: 'code', text: 'c' },
    { kind: 'text', text: ' ' },
    { kind: 'link', href: 'http://e', children: [{ kind: 'text', text: 't' }] },
    { kind: 'text', text: ' ' },
    { kind: 'wikilink', target: 'Note', label: 'lbl' },
    { kind: 'text', text: ' ' },
    { kind: 'tag', text: '#tag' },
    { kind: 'text', text: ' ' },
    { kind: 'math', tex: 'x^2' },
    { kind: 'text', text: ' a|b' },
  ])
  // The offset a click jumps to is the cell's own source.
  assert.equal(src.slice(cell.at, cell.at + 5), '**b**')
})
