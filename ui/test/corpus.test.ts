// Cross-parser conformance: every corpus/*.md must index to exactly corpus/*.json, the same
// fixtures `cargo test -p lemmate-core corpus` checks against the Rust indexer.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { index, type NoteIndex } from '../src/markdown/index.ts'

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
