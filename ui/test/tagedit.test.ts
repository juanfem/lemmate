// Adding a tag rewrites YAML somebody else wrote by hand, so every case is checked by *reading
// the result back through the real front-matter parser* rather than by comparing strings: what
// matters is that the note now carries the tag and still carries everything it carried before.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { addTagToFrontMatter, cleanTag } from '../src/lib/tagedit.ts'
import { frontMatter } from '../src/markdown/frontmatter.ts'

/** Apply the edit the way CodeMirror would. */
function add(doc: string, tag: string): string {
  const edit = addTagToFrontMatter(doc, tag)
  assert.ok(edit, 'expected an edit')
  return doc.slice(0, edit.from) + edit.insert + doc.slice(edit.to)
}

test('a note with no front matter gets one', () => {
  const out = add('# Title\n\nbody\n', 'reference')
  assert.deepEqual(frontMatter(out).tags, ['reference'])
  assert.ok(out.endsWith('# Title\n\nbody\n'), out)
})

test('front matter without a tags key gains one', () => {
  const doc = '---\nid: 01J8Z9\ntitle: Handbook\n---\n\n# Handbook\n'
  const out = add(doc, 'reference')
  const fm = frontMatter(out)
  assert.deepEqual(fm.tags, ['reference'])
  assert.equal(fm.title, 'Handbook', 'the keys it already had survive')
  assert.equal(fm.id, '01J8Z9')
  assert.ok(out.includes('\n# Handbook\n'), 'and so does the body')
})

test('a flow list is joined, not converted', () => {
  const out = add('---\ntags: [a, b]\n---\n\nbody\n', 'c')
  assert.deepEqual(frontMatter(out).tags, ['a', 'b', 'c'])
  assert.ok(out.includes('tags: [a, b, c]'), out)
})

test('an empty flow list does not gain a stray comma', () => {
  const out = add('---\ntags: []\n---\n', 'a')
  assert.deepEqual(frontMatter(out).tags, ['a'])
  assert.ok(out.includes('tags: [a]'), out)
})

test('a block list is followed at its own indent', () => {
  const doc = '---\ntitle: T\ntags:\n    - a\n    - b\nid: x\n---\n\nbody\n'
  const out = add(doc, 'c')
  assert.deepEqual(frontMatter(out).tags, ['a', 'b', 'c'])
  assert.ok(out.includes('    - b\n    - c\n'), out)
  assert.equal(frontMatter(out).id, 'x', 'the key after the list is not swallowed')
})

test('the comma-separated string form stays a string', () => {
  const out = add('---\ntags: a, b\n---\n', 'c')
  assert.deepEqual(frontMatter(out).tags, ['a', 'b', 'c'])
})

test('an empty tags key becomes a list', () => {
  const out = add('---\ntitle: T\ntags:\n---\n', 'a')
  assert.deepEqual(frontMatter(out).tags, ['a'])
  assert.equal(frontMatter(out).title, 'T')
})

test('a tag YAML would read as something other than a string is quoted', () => {
  // Unquoted, `no` is a boolean and `2026` a number — and one non-string voids the whole front
  // matter, which would take the note's title and id down with it.
  for (const tag of ['no', 'yes', 'true', 'off', '2026']) {
    const out = add('---\ntitle: T\ntags: [a]\n---\n', tag)
    const fm = frontMatter(out)
    assert.deepEqual(fm.tags, ['a', tag], `${tag} survived as a tag`)
    assert.equal(fm.title, 'T', `${tag} did not void the front matter`)
  }
})

test('an ordinary tag is not quoted', () => {
  assert.ok(add('---\ntags: [a]\n---\n', 'team/process').includes('[a, team/process]'))
})

test('cleanTag takes what someone would actually type', () => {
  assert.equal(cleanTag('#Reference'), 'reference')
  assert.equal(cleanTag('  project alpha '), 'project-alpha')
  assert.equal(cleanTag('a "b" c!'), 'a-b-c')
  assert.equal(cleanTag('/nested/'), 'nested')
  assert.equal(cleanTag('!!!'), '', 'nothing usable left')
  assert.equal(cleanTag(''), '')
})

test('nothing is written for a tag that cleaned away to nothing', () => {
  assert.equal(addTagToFrontMatter('---\ntags: [a]\n---\n', ''), null)
})
