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
