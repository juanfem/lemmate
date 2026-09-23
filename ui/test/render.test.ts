import { test } from 'node:test'
import assert from 'node:assert/strict'
import { beforeBodyEnd } from '../src/lib/render.ts'

test('the script goes before the last </body>, not one inside a script', () => {
  const deck = '<html><body><script>w.document.write("<html><body></body></html>")</script></body></html>'
  assert.equal(
    beforeBodyEnd(deck, '<s/>'),
    '<html><body><script>w.document.write("<html><body></body></html>")</script><s/></body></html>',
  )
  assert.equal(beforeBodyEnd('no body', '<s/>'), 'no body<s/>')
})
