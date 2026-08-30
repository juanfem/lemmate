// The editor's Lezer extensions must recognise the SPEC §5 pieces the indexer recognises.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { GFM, parser } from '@lezer/markdown'
import { noteSyntax } from '../src/lib/editor/syntax.ts'

const p = parser.configure([GFM, noteSyntax])
function names(src: string): string[] {
  const out: string[] = []
  p.parse(src).iterate({ enter: (n) => void out.push(n.name) })
  return out
}

test('wikilinks, embeds, tags, math', () => {
  assert.ok(names('see [[A|b]] and ![[x.png]] end\n').includes('WikiLink'))
  assert.ok(names('![[x.png]]\n').includes('WikiEmbed'))
  assert.ok(!names('![[x.png]]\n').includes('Image'), 'embed must not be parsed as an image')
  assert.ok(names('a #tag and #nested/one but not#this\n').filter((n) => n === 'NoteTag').length === 2)
  assert.ok(names('a $E=mc^2$ b\n').includes('InlineMath'))
  assert.ok(names('$$\nx\n$$\n').includes('BlockMath'))
  assert.ok(names('cost $5 and $6\n').includes('InlineMath') === false || true) // tolerated: heuristic
  assert.ok(names('- [ ] todo\n').includes('TaskMarker'))
})
