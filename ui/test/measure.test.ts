// Full-width text is a per-browser preference: stored when on, absent when off, and never a
// reason to fail when storage is out of reach.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { MEASURE_KEY, readFullWidth, writeFullWidth } from '../src/lib/measure.ts'

function memory() {
  const m = new Map<string, string>()
  return {
    m,
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
  }
}

test('the centred column is the default, and full width round-trips', () => {
  const s = memory()
  assert.equal(readFullWidth(s), false)
  writeFullWidth(s, true)
  assert.equal(s.m.get(MEASURE_KEY), 'full')
  assert.equal(readFullWidth(s), true)
  writeFullWidth(s, false)
  assert.equal(s.m.has(MEASURE_KEY), false)
  assert.equal(readFullWidth(s), false)
})

test('storage that throws or is missing reads as the default', () => {
  const broken = {
    getItem: () => {
      throw new Error('denied')
    },
    setItem: () => {
      throw new Error('denied')
    },
    removeItem: () => {
      throw new Error('denied')
    },
  }
  assert.equal(readFullWidth(broken), false)
  assert.doesNotThrow(() => writeFullWidth(broken, true))
  assert.equal(readFullWidth(undefined), false)
})
