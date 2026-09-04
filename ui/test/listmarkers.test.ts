// Live preview renders a list's marker as the shape (or the number) its nesting level and its
// position call for, rather than the `-` or the digits the source happens to hold.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { GFM, parser } from '@lezer/markdown'
import type { SyntaxNode } from '@lezer/common'
import { noteSyntax } from '../src/lib/editor/syntax.ts'
import { listBullet, listNumber } from '../src/lib/editor/livePreview.ts'

const p = parser.configure([GFM, noteSyntax])

const depthOf = (list: SyntaxNode): number => {
  let depth = 0
  for (let a: SyntaxNode | null = list; a; a = a.parent) if (a.name === list.name) depth++
  return depth
}

/**
 * What the decoration builder draws over every list marker in `src`, in document order, with
 * `null` where the source is left showing through.
 */
function markers(src: string): (string | null)[] {
  const drawn = new Map<number, string | null>()
  p.parse(src).iterate({
    enter: (node) => {
      const n = node.node
      if (node.name === 'ListMark') {
        const item = n.parent
        if (item?.parent?.name !== 'BulletList') return
        drawn.set(n.from, listBullet(depthOf(item.parent), item.getChild('Task') !== null))
      } else if (node.name === 'OrderedList') {
        const depth = depthOf(n)
        let start: number | null = null
        let index = 0
        for (const li of n.getChildren('ListItem')) {
          const mark = li.getChild('ListMark')
          if (!mark) continue
          const m = /^(\d+)([.)])$/u.exec(src.slice(mark.from, mark.to))
          if (!m) continue
          start ??= Number(m[1])
          const label = listNumber(depth, start + index++)
          const text = label === null ? null : label + m[2]!
          drawn.set(mark.from, text === m[0] ? null : text)
        }
      }
    },
  })
  return [...drawn.entries()].sort((a, b) => a[0] - b[0]).map(([, text]) => text)
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
  assert.equal(listNumber(1, 3), '3')
  assert.equal(listNumber(2, 3), 'c')
  assert.equal(listNumber(3, 3), 'iii')
  assert.equal(listNumber(4, 3), '3')
})

test('alpha and roman go past the easy cases', () => {
  assert.equal(listNumber(2, 26), 'z')
  assert.equal(listNumber(2, 27), 'aa')
  assert.equal(listNumber(2, 53), 'ba')
  assert.equal(listNumber(3, 4), 'iv')
  assert.equal(listNumber(3, 1949), 'mcmxlix')
})

test('a number the numerals cannot spell keeps its digits', () => {
  assert.equal(listNumber(2, 0), null)
  assert.equal(listNumber(3, 4000), null)
})

test('an ordered list is renumbered by level, delimiter and all', () => {
  const src = '1. one\n2. two\n    1. sub\n    2. sub\n        7) deep\n'
  assert.deepEqual(markers(src), [null, null, 'a.', 'b.', 'vii)'])
})

test('the number is the position in the list, not the digits in the file', () => {
  // What every markdown renderer does — and what makes `1.` on every line work.
  assert.deepEqual(markers('1. one\n1. two\n1. three\n'), [null, '2.', '3.'])
  // A list starts at its first item's number, so the gap an indented-away item leaves closes.
  assert.deepEqual(markers('1. one\n3. two\n'), [null, '2.'])
  assert.deepEqual(markers('5. five\n5. six\n'), [null, '6.'])
})

test('the two kinds of list count their own nesting', () => {
  // A bullet under an ordered item is a first-level bullet, and vice versa.
  assert.deepEqual(markers('1. one\n   - two\n'), [null, '•'])
  assert.deepEqual(markers('- one\n    1. two\n'), ['•', null])
  assert.deepEqual(markers('- one\n    1. two\n        1. three\n'), ['•', null, 'a.'])
})
