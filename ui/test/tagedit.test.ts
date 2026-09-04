// Adding a tag rewrites YAML somebody else wrote by hand, so every case is checked by *reading
// the result back through the real front-matter parser* rather than by comparing strings: what
// matters is that the note now carries the tag and still carries everything it carried before.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { addTagToFrontMatter, cleanTag, removeTagFromText, renameTagInText } from '../src/lib/tagedit.ts'
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

// ---- renaming and removing, which have to reach the prose as well as the front matter

test('renaming reaches the front matter and the sentences', () => {
  const doc = '---\ntags: [projects, other]\n---\n\nSee #projects and #other, plus #projects/alpha.\n'
  const out = renameTagInText(doc, 'projects', 'work')
  assert.ok(out)
  assert.deepEqual(frontMatter(out).tags, ['work', 'other'])
  assert.ok(out.includes('See #work and #other, plus #work/alpha.'), out)
})

test('a nested tag follows its parent, in the list too', () => {
  const doc = '---\ntags:\n  - projects/alpha\n  - projects\n---\n\nbody\n'
  const out = renameTagInText(doc, 'projects', 'work')
  assert.ok(out)
  assert.deepEqual(frontMatter(out).tags, ['work/alpha', 'work'])
})

test('renaming onto a tag the note already has does not duplicate it', () => {
  const out = renameTagInText('---\ntags: [a, b]\n---\n', 'a', 'b')
  assert.ok(out)
  assert.deepEqual(frontMatter(out).tags, ['b'])
})

test('a note that does not carry the tag is left alone', () => {
  assert.equal(renameTagInText('---\ntags: [a]\n---\n\n#a is here\n', 'zzz', 'q'), null)
  assert.equal(removeTagFromText('---\ntags: [a]\n---\n\nbody\n', 'zzz'), null)
  assert.equal(renameTagInText('---\ntags: [a]\n---\n', 'a', 'a'), null, 'renaming to itself')
})

test('removing closes the gap the tag leaves in a sentence', () => {
  const out = removeTagFromText('---\ntags: [a]\n---\n\nAn inline #a tag here.\n', 'a')
  assert.ok(out)
  assert.deepEqual(frontMatter(out).tags, [])
  assert.ok(out.includes('An inline tag here.'), JSON.stringify(out))
})

test('removing takes the space behind it when the tag opened the line', () => {
  const out = removeTagFromText('#a and more\n', 'a')
  assert.equal(out, 'and more\n')
})

test('a tag that was the whole line leaves the line empty, not a space', () => {
  const out = removeTagFromText('one\n#a\ntwo\n', 'a')
  assert.equal(out, 'one\n\ntwo\n')
})

test('an emptied list takes its key with it', () => {
  const out = removeTagFromText('---\ntitle: T\ntags: [a]\nid: x\n---\n\nbody\n', 'a')
  assert.equal(out, '---\ntitle: T\nid: x\n---\n\nbody\n')
  assert.equal(frontMatter(out).title, 'T')
})

test('an emptied block list takes all of its lines', () => {
  const out = removeTagFromText('---\ntags:\n  - a\n  - b\nid: x\n---\n', 'a')
  assert.ok(out)
  assert.deepEqual(frontMatter(out).tags, ['b'])
  const gone = removeTagFromText(out, 'b')
  assert.equal(gone, '---\nid: x\n---\n')
})

test('a nested tag is not removed with its parent', () => {
  const out = removeTagFromText('---\ntags: [projects, projects/alpha]\n---\n\n#projects #projects/alpha\n', 'projects')
  assert.ok(out)
  assert.deepEqual(frontMatter(out).tags, ['projects/alpha'])
  assert.ok(out.includes('#projects/alpha'), out)
  assert.ok(!/#projects\s/u.test(out), out)
})

test('a #tag inside code is prose about a tag, not a tag', () => {
  const doc = [
    '---',
    'tags: [a]',
    '---',
    '',
    'Real #a here.',
    '',
    'Inline `#a` stays.',
    '',
    '```sh',
    'grep #a file',
    '```',
    '',
    'End.',
    '',
  ].join('\n')
  const renamed = renameTagInText(doc, 'a', 'b')
  assert.ok(renamed)
  assert.ok(renamed.includes('Real #b here.'), 'the real one moved')
  assert.ok(renamed.includes('Inline `#a` stays.'), 'the code span did not')
  assert.ok(renamed.includes('grep #a file'), 'nor did the fence')
})

test('the front matter is not treated as prose', () => {
  // `tags: [a]` holds no `#`, but a title might, and rewriting one would be wrong.
  const doc = '---\ntitle: "Notes on #a"\ntags: [a]\n---\n\n#a\n'
  const out = renameTagInText(doc, 'a', 'b')
  assert.ok(out)
  assert.equal(frontMatter(out).title, 'Notes on #a', 'the title is left as written')
  assert.deepEqual(frontMatter(out).tags, ['b'])
})

test('a word ending in a hash is not a tag', () => {
  assert.equal(renameTagInText('C# and #a\n', 'a', 'b'), 'C# and #b\n')
})
