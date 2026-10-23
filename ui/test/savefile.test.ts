import { test } from 'node:test'
import assert from 'node:assert/strict'
import { saveAs, type SaveHandle, type SavePicker } from '../src/lib/savefile.ts'

/** A picker that answers with `handle` (or cancels), and what it was asked. */
function picker(handle: SaveHandle | null) {
  const asked: Parameters<SavePicker>[0][] = []
  const pick: SavePicker = async (o) => {
    asked.push(o)
    if (!handle) throw new DOMException('cancelled', 'AbortError')
    return handle
  }
  return { pick, asked }
}

function file() {
  const written: Blob[] = []
  let removed = false
  const handle: SaveHandle = {
    createWritable: async () => ({ write: async (b) => void written.push(b), close: async () => {} }),
    remove: async () => void (removed = true),
  }
  return { handle, written, removed: () => removed }
}

const PDF = 'application/pdf'

test('with a picker the place is asked before the file is made, then written there', async () => {
  const f = file()
  const p = picker(f.handle)
  const order: string[] = []
  const pick: SavePicker = (o) => (order.push('pick'), p.pick(o))
  const blob = new Blob(['%PDF'])
  assert.equal(await saveAs('Deck.pdf', PDF, async () => (order.push('make'), blob), { picker: pick, fallback: () => assert.fail() }), true)
  assert.deepEqual(order, ['pick', 'make'])
  assert.deepEqual(p.asked, [{ suggestedName: 'Deck.pdf', types: [{ accept: { [PDF]: ['.pdf'] } }] }])
  assert.deepEqual(f.written, [blob])
})

test('a cancelled picker makes nothing', async () => {
  let made = false
  assert.equal(await saveAs('Deck.pdf', PDF, async () => ((made = true), new Blob()), { picker: picker(null).pick }), false)
  assert.equal(made, false)
})

test('a file that could not be made, or had nothing in it, leaves no empty file behind', async () => {
  const failed = file()
  await assert.rejects(saveAs('Deck.pdf', PDF, async () => { throw new Error('no chrome') }, { picker: picker(failed.handle).pick }), /no chrome/u)
  assert.ok(failed.removed())
  const nothing = file()
  assert.equal(await saveAs('Deck.pdf', PDF, async () => null, { picker: picker(nothing.handle).pick }), false)
  assert.ok(nothing.removed() && nothing.written.length === 0)
})

test('without a picker it is a download', async () => {
  const got: [Blob, string][] = []
  const blob = new Blob(['x'])
  assert.equal(await saveAs('Deck.pptx', 'x/y', async () => blob, { picker: undefined, fallback: (b, n) => got.push([b, n]) }), true)
  assert.deepEqual(got, [[blob, 'Deck.pptx']])
})
