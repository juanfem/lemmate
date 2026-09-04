// Live preview renders a list's marker as the shape (or the numbering) its nesting level calls
// for, rather than the `-` or the digits the source happens to hold.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { GFM, parser } from '@lezer/markdown'
import type { SyntaxNode } from '@lezer/common'
import { noteSyntax } from '../src/lib/editor/syntax.ts'
import { listBullet, listNumber } from '../src/lib/editor/livePreview.ts'

const p = parser.configure([GFM, noteSyntax])

/**
 * What the decoration builder does to every list marker in `src`: the text it renders as, or
 * `null` where the source is left showing through.
 */
function markers(src: string): (string | null)[] {
  const out: (string | null)[] = []
  p.parse(src).iterate({
    enter: (node) => {
      if (node.name !== 'ListMark') return
      const item = node.node.parent
      const list = item?.parent
      if (!item || !list) return
      let depth = 0
      for (let a: SyntaxNode | null = list; a; a = a.parent) if (a.name === list.name) depth++
      if (list.name === 'BulletList') {
        out.push(listBullet(depth, item.getChild('Task') !== null))
      } else if (list.name === 'OrderedList') {
        const m = /^(\d+)([.)])$/u.exec(src.slice(node.from, node.to))
        const label = m ? listNumber(depth, Number(m[1])) : null
        out.push(label === null ? null : label + m![2]!)
      }
    },
  })
  return out
}

test('each bullet level gets its own shape, cycling past three', () => {
  assert.equal(listBullet(1, false), '•')
  assert.equal(listBullet(2, false), '◦')
  assert.equal(listBullet(3, false), '▪')
  assert.equal(listBullet(4, false), listBullet(1, false))
  assert.equal(listBullet(0, false), listBullet(1, false))
})

test('bullet nesting depth comes from the BulletList ancestors', () => {
  const src = '- one\n  - two\n    - three\n      - four\n- back\n'
  assert.deepEqual(markers(src), ['•', '◦', '▪', '•', '•'])
})

test('a task item shows no bullet — the checkbox is its marker', () => {
  assert.deepEqual(markers('- [ ] todo\n  - [x] done\n'), ['', ''])
  // Only the task item loses its bullet; a plain sibling keeps one.
  assert.deepEqual(markers('- [ ] todo\n- plain\n'), ['', '•'])
})

test('ordered levels run decimal, alpha, roman', () => {
  assert.equal(listNumber(1, 3), null) // the digits are already the marker
  assert.equal(listNumber(2, 3), 'c')
  assert.equal(listNumber(3, 3), 'iii')
  assert.equal(listNumber(4, 3), null)
})

test('alpha and roman go past the easy cases', () => {
  assert.equal(listNumber(2, 26), 'z')
  assert.equal(listNumber(2, 27), 'aa')
  assert.equal(listNumber(2, 53), 'ba')
  assert.equal(listNumber(3, 4), 'iv')
  assert.equal(listNumber(3, 1949), 'mcmxlix')
})

test('a number outside the range keeps its digits', () => {
  assert.equal(listNumber(2, 0), null)
  assert.equal(listNumber(3, 4000), null)
})

test('an ordered list is renumbered by level, delimiter and all', () => {
  const src = '1. one\n2. two\n    1. sub\n    2. sub\n        7) deep\n'
  assert.deepEqual(markers(src), [null, null, 'a.', 'b.', 'vii)'])
})

test('the two kinds of list count their own nesting', () => {
  // A bullet under an ordered item is a first-level bullet, and vice versa.
  assert.deepEqual(markers('1. one\n   - two\n'), [null, '•'])
  assert.deepEqual(markers('- one\n    1. two\n'), ['•', null])
  assert.deepEqual(markers('- one\n    1. two\n        1. three\n'), ['•', null, 'a.'])
})
