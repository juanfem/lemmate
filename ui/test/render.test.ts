import { test } from 'node:test'
import assert from 'node:assert/strict'
import { beforeBodyEnd, keepRender, keptRender } from '../src/lib/render.ts'

test('the script goes before the last </body>, not one inside a script', () => {
  const deck = '<html><body><script>w.document.write("<html><body></body></html>")</script></body></html>'
  assert.equal(
    beforeBodyEnd(deck, '<s/>'),
    '<html><body><script>w.document.write("<html><body></body></html>")</script><s/></body></html>',
  )
  assert.equal(beforeBodyEnd('no body', '<s/>'), 'no body<s/>')
})

test('a kept render comes back by vault and note, and only the most recent few are kept', () => {
  const r = (html: string) => ({ html, made: 'page', renderId: '', choice: 'auto', renderedFrom: 't' })
  keepRender('v', 'a', r('A'))
  assert.equal(keptRender('v', 'a')?.html, 'A')
  assert.equal(keptRender('w', 'a'), undefined)
  keepRender('v', 'a', r('A2'))
  assert.equal(keptRender('v', 'a')?.html, 'A2')
  // Eight more, but `a` is shown again half-way: it outlives the ones shown longer ago.
  for (let i = 0; i < 4; i++) keepRender('v', `n${i}`, r(''))
  keptRender('v', 'a')
  for (let i = 4; i < 8; i++) keepRender('v', `n${i}`, r(''))
  assert.equal(keptRender('v', 'a')?.html, 'A2')
  assert.equal(keptRender('v', 'n0'), undefined)
})
