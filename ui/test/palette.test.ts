// The palette is a per-browser preference: Finder unless warm paper is stored, and never a
// reason to fail when storage is out of reach.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { PAGE_BG, PALETTE_KEY, readPalette, writePalette } from '../src/lib/palette.ts'

function memory() {
  const m = new Map<string, string>()
  return {
    m,
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
  }
}

test('Finder is the default, and warm paper round-trips', () => {
  const s = memory()
  assert.equal(readPalette(s), 'finder')
  writePalette(s, 'warm')
  assert.equal(s.m.get(PALETTE_KEY), 'warm')
  assert.equal(readPalette(s), 'warm')
  writePalette(s, 'finder')
  assert.equal(s.m.has(PALETTE_KEY), false)
  assert.equal(readPalette(s), 'finder')
})

test('an unknown value, or storage that throws or is missing, reads as Finder', () => {
  const s = memory()
  s.m.set(PALETTE_KEY, 'solarized')
  assert.equal(readPalette(s), 'finder')
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
  assert.equal(readPalette(broken), 'finder')
  assert.doesNotThrow(() => writePalette(broken, 'warm'))
  assert.equal(readPalette(undefined), 'finder')
})

test('the browser-chrome colours match --bg in app.css', () => {
  const css = readFileSync(new URL('../src/app.css', import.meta.url), 'utf8')
  // Each palette block's first --bg, in the order app.css declares them.
  const bgs = [...css.matchAll(/--bg:\s*(#[0-9a-f]+)/giu)].map((m) => m[1]!.toLowerCase())
  assert.deepEqual(bgs, [PAGE_BG.finder.light, PAGE_BG.warm.light, PAGE_BG.finder.dark, PAGE_BG.warm.dark])
})
