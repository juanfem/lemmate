// Spell checking is a per-browser preference: on unless off is stored, and never a reason to
// fail when storage is out of reach.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { SPELLCHECK_KEY, readSpellcheck, writeSpellcheck } from '../src/lib/spellcheck.ts'

function memory() {
  const m = new Map<string, string>()
  return {
    m,
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
  }
}

test('on is the default, and off round-trips', () => {
  const s = memory()
  assert.equal(readSpellcheck(s), true)
  writeSpellcheck(s, false)
  assert.equal(s.m.get(SPELLCHECK_KEY), 'off')
  assert.equal(readSpellcheck(s), false)
  writeSpellcheck(s, true)
  assert.equal(s.m.has(SPELLCHECK_KEY), false)
  assert.equal(readSpellcheck(s), true)
})

test('storage that throws or is missing reads as on, and writing to it does not throw', () => {
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
  assert.equal(readSpellcheck(broken), true)
  assert.equal(readSpellcheck(undefined), true)
  writeSpellcheck(broken, false)
  writeSpellcheck(undefined, false)
})
