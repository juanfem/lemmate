// CodeMirror's height map measures every line and block widget with `getBoundingClientRect()`,
// which reports the *border box* — margins are outside it. A vertical margin anywhere the
// editor measures therefore adds screen space the height map never learns about, and since
// `posAtCoords` picks the line purely from that map, the error accumulates down the document:
// clicks land on the wrong line, drag-selection grabs the wrong text, and Up/Down skip lines.
// Horizontal margins are harmless — only the vertical ones desynchronise the map.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'

const source = readFileSync(new URL('../src/lib/editor/setup.ts', import.meta.url), 'utf8')
const themeBody = /EditorView\.theme\(\{([\s\S]*?)\n\}\)/u.exec(source)?.[1]

const ZERO = /^-?0(?:px|r?em|%)?$/u

/** The top and bottom of a margin declaration, whatever shorthand it was written in. */
function verticals(prop: string, value: string): string[] {
  const parts = value.trim().split(/\s+/u)
  if (prop === 'marginTop' || prop === 'marginBottom') return parts
  // 1 value → all four; 2 → vertical horizontal; 3 → top horizontal bottom; 4 → t r b l.
  return [parts[0]!, parts.length >= 3 ? parts[2]! : parts[0]!]
}

test('the editor theme declares no vertical margins', () => {
  assert.ok(themeBody, 'could not find the EditorView.theme({…}) block in setup.ts')
  const offenders: string[] = []
  for (const line of themeBody.split('\n')) {
    const rule = /^\s*'([^']+)':\s*\{(.*)\},?\s*$/u.exec(line)
    if (!rule) continue
    for (const [, prop, value] of rule[2]!.matchAll(/(margin(?:Top|Bottom)?)\s*:\s*'([^']*)'/gu)) {
      // A margin that adds no vertical space is not the bug: `margin: '0 auto'` centres,
      // `margin: '0 -0.5rem'` bleeds sideways, and `margin: '0'` cancels a browser default —
      // all leave the height map alone. `marginRight`/`marginLeft` never match at all.
      if (verticals(prop!, value!).every((v) => ZERO.test(v))) continue
      offenders.push(`${rule[1]} { ${prop}: ${value} }`)
    }
  }
  assert.deepEqual(offenders, [], `vertical margins break CodeMirror's height map:\n  ${offenders.join('\n  ')}`)
})
