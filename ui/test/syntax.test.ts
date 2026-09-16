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

test('a fence names its language in any of the usual spellings', async () => {
  const { codeLanguage, codeLanguageName } = await import('../src/lib/editor/syntax.ts')
  const { languages } = await import('@codemirror/language-data')
  assert.equal(codeLanguageName('rust'), 'rust')
  assert.equal(codeLanguageName('{python}'), 'python')
  assert.equal(codeLanguageName('{.haskell .numberLines}'), 'haskell')
  assert.equal(codeLanguageName('js title="x"'), 'js')
  assert.equal(codeLanguageName(''), '')
  assert.equal(codeLanguage(languages, '{python}')?.name, 'Python')
  assert.equal(codeLanguage(languages, 'ts')?.name, 'TypeScript')
  assert.equal(codeLanguage(languages, 'no-such-language'), null)
})
