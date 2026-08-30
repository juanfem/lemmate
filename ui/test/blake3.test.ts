import { test } from 'node:test'
import assert from 'node:assert/strict'
import { blake3Hex } from '../src/lib/blake3.ts'

test('blake3 matches the reference vectors used by the Rust side', async () => {
  assert.equal(await blake3Hex(new Uint8Array()), 'af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262')
  assert.equal(await blake3Hex(new TextEncoder().encode('abc')), '6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85')
})
