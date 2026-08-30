import { test } from 'node:test'
import assert from 'node:assert/strict'
import { decodeFrame, encodeFrame } from '../src/lib/frames.ts'
import { ULID_RE, ulid } from '../src/lib/ulid.ts'

test('frame codec round-trips and rejects junk', () => {
  const payload = new Uint8Array([0, 1, 2, 250])
  const bytes = encodeFrame('vault:01ARZ3NDEKTSV4RRFFQ69G5FAV', payload)
  const back = decodeFrame(bytes)
  assert.equal(back.docId, 'vault:01ARZ3NDEKTSV4RRFFQ69G5FAV')
  assert.deepEqual(Array.from(back.payload), Array.from(payload))
  assert.throws(() => decodeFrame(new Uint8Array([0])))
  assert.throws(() => decodeFrame(new Uint8Array([0, 9, 65])))
})

test('ulids are well-formed, unique, and time-ordered', () => {
  const a = ulid(1_000_000)
  const b = ulid(2_000_000)
  assert.match(a, ULID_RE)
  assert.match(b, ULID_RE)
  assert.ok(a.slice(0, 10) < b.slice(0, 10), 'time prefix sorts')
  const many = new Set(Array.from({ length: 200 }, () => ulid()))
  assert.equal(many.size, 200)
})

test('wikilink rewrite mirrors the Rust rules', async () => {
  const { rewriteWikilinks } = await import('../src/lib/links.ts')
  assert.equal(
    rewriteWikilinks('see [[Projects/Plan]] and [[Plan|the plan]] and [[Projects/Plan.md#Goals]] but not [[Planning]]', 'Projects/Plan.md', 'Archive/Roadmap.md'),
    'see [[Archive/Roadmap]] and [[Roadmap|the plan]] and [[Archive/Roadmap#Goals]] but not [[Planning]]',
  )
  assert.equal(rewriteWikilinks('[[Plan]] [[Projects/Plan]]', 'Projects/Plan.md', 'Done/Plan.md'), '[[Plan]] [[Done/Plan]]')
  assert.equal(rewriteWikilinks('nothing', 'a.md', 'b.md'), null)
})
