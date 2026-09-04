// Live preview renders a bullet list's `-` as a shape that depends on the nesting level.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { GFM, parser } from '@lezer/markdown'
import type { SyntaxNode } from '@lezer/common'
import { noteSyntax } from '../src/lib/editor/syntax.ts'
import { listBullet } from '../src/lib/editor/livePreview.ts'

const p = parser.configure([GFM, noteSyntax])

/** What the decoration builder does: the bullet each `-` in a bullet list renders as. */
function bullets(src: string): string[] {
  const out: string[] = []
  p.parse(src).iterate({
    enter: (node) => {
      if (node.name !== 'ListMark') return
      const item = node.node.parent
      const list = item?.parent
      if (!item || list?.name !== 'BulletList') return
      let depth = 0
      for (let a: SyntaxNode | null = list; a; a = a.parent) if (a.name === 'BulletList') depth++
      out.push(listBullet(depth, item.getChild('Task') !== null))
    },
  })
  return out
}

test('each level gets its own shape, cycling past three', () => {
  assert.equal(listBullet(1, false), '•')
  assert.equal(listBullet(2, false), '◦')
  assert.equal(listBullet(3, false), '▪')
  assert.equal(listBullet(4, false), listBullet(1, false))
  assert.equal(listBullet(0, false), listBullet(1, false))
})

test('nesting depth comes from the BulletList ancestors', () => {
  const src = '- one\n  - two\n    - three\n      - four\n- back\n'
  assert.deepEqual(bullets(src), ['•', '◦', '▪', '•', '•'])
})

test('ordered list markers are left alone', () => {
  assert.deepEqual(bullets('1. one\n2. two\n'), [])
  // A bullet nested under an ordered item still counts only its bullet ancestors.
  assert.deepEqual(bullets('1. one\n   - two\n'), ['•'])
})

test('a task item shows no bullet — the checkbox is its marker', () => {
  assert.deepEqual(bullets('- [ ] todo\n  - [x] done\n'), ['', ''])
  // Only the task item loses its bullet; a plain sibling keeps one.
  assert.deepEqual(bullets('- [ ] todo\n- plain\n'), ['', '•'])
})
