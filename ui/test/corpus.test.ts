// Cross-parser conformance: every corpus/*.md must index to exactly corpus/*.json, the same
// fixtures `cargo test -p lemmate-core corpus` checks against the Rust indexer.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { index, MAX_CONTAINERS, MAX_EMPHASIS, MAX_INDENT, tame, type NoteIndex } from '../src/markdown/index.ts'

const corpus = join(dirname(fileURLToPath(import.meta.url)), '..', '..', 'corpus')
const cases = readdirSync(corpus).filter((f) => f.endsWith('.md') && f !== 'README.md')
assert.ok(cases.length > 0, 'corpus is empty')

for (const file of cases) {
  test(`corpus: ${file}`, () => {
    const expected = JSON.parse(readFileSync(join(corpus, file.replace(/\.md$/u, '.json')), 'utf8')) as NoteIndex
    const got = index(readFileSync(join(corpus, file), 'utf8'))
    got.plain_text = ''
    expected.plain_text = ''
    assert.deepEqual(got, expected)
  })
}

test('plain text is searchable prose', () => {
  const ix = index('# Title\n\nSome *emphasis* and `code` with $x$.\n')
  assert.equal(ix.plain_text, 'Title Some emphasis and with .')
})

test('notes nested ten thousand deep index without overflowing or hanging', () => {
  const r = (s: string, n: number) => s.repeat(n)
  const cases: [string, string][] = [
    ['quotes', `${r('>', 10_000)} x #end\n`],
    ['emphasis', `${r('*', 10_000)}x${r('*', 10_000)} #end\n`],
    ['spaced emphasis', `${r('*a ', 10_000)}x${r(' a*', 10_000)} #end\n`],
    ['strikethrough', `${r('~~a ', 5_000)}x${r(' a~~', 5_000)} #end\n`],
    ['brackets', `${r('[', 10_000)}x${r(']', 10_000)} #end\n`],
    ['images', `${r('![', 10_000)}x${r('](u)', 10_000)} #end\n`],
    ['list markers', `${r('- ', 10_000)}x #end\n`],
    ['indented lists', Array.from({ length: 300 }, (_, i) => `${r('  ', i)}- x\n`).join('') + '\n#end\n'],
    ['quote lines', Array.from({ length: 300 }, (_, i) => `${r('> ', i)}x\n`).join('') + '\n#end\n'],
  ]
  for (const [name, src] of cases) {
    const started = Date.now()
    const ix = index(src)
    assert.ok(Date.now() - started < 20_000, `${name}: ${Date.now() - started} ms`)
    assert.ok(ix.tags.includes('end'), `${name}: ${ix.tags.join(',')}`)
  }
})

test('taming leaves ordinary notes alone and escapes past the limits', () => {
  const note = '---\ntitle: T\n---\n> > quoted *em* _em_ snake_case [[link]] [a](b)\n\n- a\n  - b\n'
  assert.equal(tame(note), note)
  assert.equal(tame(`${'>'.repeat(MAX_CONTAINERS + 5)}x\n`), `${'>'.repeat(MAX_CONTAINERS)}\\>${'>'.repeat(4)}x\n`)
  assert.equal(tame(`${'*'.repeat(MAX_EMPHASIS + 1)}\\*\n\n*a*\n`), `${'*'.repeat(MAX_EMPHASIS)}\\*\\*\n\n*a*\n`)
  assert.equal(tame(`${' '.repeat(MAX_INDENT + 50)}- x\n`), `${' '.repeat(MAX_INDENT)}- x\n`)
})

test('URLs hold no tags; tags are composed; scalar titles are text', () => {
  const ix = index(
    'https://example.com/#anchor <https://a.b/#c> www.x.org/#w <me@x.org> [label #kept](https://y.z/#gone) #real\n',
  )
  assert.deepEqual(ix.tags, ['kept', 'real'])
  assert.deepEqual(index('#café and #áb\n').tags, ['café', 'áb'])
  const fm = index('---\ntitle: 2024\nid: 0123\n---\n# H\n')
  assert.equal(fm.title, '2024')
  assert.equal(fm.front_matter?.id, '0123')
})
