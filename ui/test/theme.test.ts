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

test('the editor theme declares no vertical margins', () => {
  assert.ok(themeBody, 'could not find the EditorView.theme({…}) block in setup.ts')
  const offenders: string[] = []
  for (const line of themeBody.split('\n')) {
    const rule = /^\s*'([^']+)':\s*\{(.*)\},?\s*$/u.exec(line)
    if (!rule) continue
    for (const [, prop, value] of rule[2]!.matchAll(/(margin(?:Top|Bottom)?)\s*:\s*'([^']*)'/gu)) {
      // `margin: '0 auto'` and `marginRight`/`marginLeft` only move things sideways.
      if (prop === 'margin' && /^\S+\s+\S+$/u.test(value!.trim()) && value!.trim().split(/\s+/u)[0] === '0') continue
      offenders.push(`${rule[1]} { ${prop}: ${value} }`)
    }
  }
  assert.deepEqual(offenders, [], `vertical margins break CodeMirror's height map:\n  ${offenders.join('\n  ')}`)
})
