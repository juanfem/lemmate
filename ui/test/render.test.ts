import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  autoRenderDue,
  beforeBodyEnd,
  keepPlace,
  keepRender,
  keptRender,
  placeOf,
  readAutoRender,
  writeAutoRender,
  type Place,
} from '../src/lib/render.ts'

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

test('a place from the frame is numbers only, and the script starts there', () => {
  assert.deepEqual(placeOf({ lemmateRenderPlace: { y: 420 } }), { y: 420 })
  assert.deepEqual(placeOf({ lemmateRenderPlace: { slide: { indexh: 3, indexv: 0, indexf: 1, x: 'no' } } }), {
    slide: { indexh: 3, indexv: 0, indexf: 1 },
  })
  assert.deepEqual(placeOf({ lemmateRenderPlace: { slide: { indexh: 2, indexv: 1 } } }), { slide: { indexh: 2, indexv: 1 } })
  for (const bad of [null, 'x', {}, { lemmateRenderPlace: { y: '</script>' } }, { lemmateRenderPlace: { y: -1 } }, { lemmateDeck: 1 }])
    assert.equal(placeOf(bad), null)
  assert.match(keepPlace({ y: 420 }), /var at=\{"y":420\};/u)
  assert.match(keepPlace(null), /var at=null;/u)
  // Smuggled past the type: still only what `placeOf` lets through.
  assert.match(keepPlace({ y: '</script><b>' } as unknown as Place), /var at=null;/u)
})

test('an automatic render is due only when on, for a page or deck, and for text not yet tried', () => {
  const due = { auto: true, visible: true, busy: false, made: 'page', text: 'b', tried: 'a' }
  assert.equal(autoRenderDue(due), true)
  assert.equal(autoRenderDue({ ...due, made: 'slides' }), true)
  assert.equal(autoRenderDue({ ...due, auto: false }), false)
  assert.equal(autoRenderDue({ ...due, visible: false }), false)
  assert.equal(autoRenderDue({ ...due, busy: true }), false)
  assert.equal(autoRenderDue({ ...due, made: 'PDF' }), false)
  assert.equal(autoRenderDue({ ...due, made: '' }), false)
  assert.equal(autoRenderDue({ ...due, text: null }), false)
  // Already rendered from this text, or a render of it failed: wait for the next edit.
  assert.equal(autoRenderDue({ ...due, tried: 'b' }), false)
})

test('the automatic-render preference is remembered, and survives storage that throws', () => {
  const m = new Map<string, string>()
  const storage = {
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
  }
  assert.equal(readAutoRender(storage), false)
  writeAutoRender(storage, true)
  assert.equal(readAutoRender(storage), true)
  writeAutoRender(storage, false)
  assert.equal(readAutoRender(storage), false)
  const broken = {
    getItem: () => {
      throw new Error('blocked')
    },
    setItem: () => {
      throw new Error('blocked')
    },
    removeItem: () => {
      throw new Error('blocked')
    },
  }
  assert.equal(readAutoRender(broken), false)
  writeAutoRender(broken, true)
  assert.equal(readAutoRender(undefined), false)
})
