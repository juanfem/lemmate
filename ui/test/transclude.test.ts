// `![[note#…]]` shows part of a note: the whole of it without front matter, the section under a
// heading, or the block a `^id` marks.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { embeddedSection, parseEmbed, stripFrontMatter } from '../src/lib/editor/transclude.ts'

const note = [
  '---',
  'id: 01J0000000000000000000000',
  'tags: [a]',
  '---',
  '# Plan',
  '',
  'Intro paragraph.',
  '',
  '## Goals',
  '',
  'Ship it. ^goal',
  '',
  '### Detail',
  'More.',
  '',
  '```sh',
  '## not a heading',
  '```',
  '',
  '## Risks',
  '',
  '- one',
  '- two ^second',
  '',
  '| a | b |',
  '|---|---|',
  '| 1 | 2 |',
  '',
  '^table',
  '',
].join('\n')

test('the embed target names a note, a heading or a block; the alias is dropped', () => {
  assert.deepEqual(parseEmbed('Plan'), { note: 'Plan' })
  assert.deepEqual(parseEmbed('Plan|shown'), { note: 'Plan' })
  assert.deepEqual(parseEmbed('Plan\\|shown'), { note: 'Plan' })
  assert.deepEqual(parseEmbed(' dir/Plan # Goals |x'), { note: 'dir/Plan', heading: 'Goals' })
  assert.deepEqual(parseEmbed('Plan#Goals#Detail'), { note: 'Plan', heading: 'Detail' })
  assert.deepEqual(parseEmbed('Plan#^goal'), { note: 'Plan', block: 'goal' })
  assert.deepEqual(parseEmbed('Plan#'), { note: 'Plan' })
})

test('front matter is not part of what an embed shows', () => {
  assert.equal(stripFrontMatter('---\nid: x\n---\nbody\n'), 'body\n')
  assert.equal(stripFrontMatter('---\nid: x\n...\nbody'), 'body')
  assert.equal(stripFrontMatter('no front matter\n---\n'), 'no front matter\n---\n')
  assert.equal(stripFrontMatter('---\nunterminated'), '---\nunterminated')
  assert.equal(embeddedSection(note, { note: 'Plan' })!.split('\n')[0], '# Plan')
  assert.ok(!embeddedSection(note, { note: 'Plan' })!.endsWith('\n'))
})

test('a heading embeds its section, down to the next heading at its level or above', () => {
  assert.equal(
    embeddedSection(note, { note: 'Plan', heading: 'goals' }),
    '## Goals\n\nShip it. ^goal\n\n### Detail\nMore.\n\n```sh\n## not a heading\n```',
  )
  assert.equal(embeddedSection(note, { note: 'Plan', heading: 'Detail' }), '### Detail\nMore.\n\n```sh\n## not a heading\n```')
  assert.ok(embeddedSection(note, { note: 'Plan', heading: 'Risks' })!.endsWith('^table'))
  assert.equal(embeddedSection('# A #\ntext\n# B\n', { note: 'x', heading: 'A' }), '# A #\ntext')
})

test('a heading that is not there, or only inside code, is reported rather than guessed', () => {
  assert.equal(embeddedSection(note, { note: 'Plan', heading: 'Missing' }), null)
  assert.equal(embeddedSection(note, { note: 'Plan', heading: 'not a heading' }), null)
  assert.equal(embeddedSection('#tag line\n', { note: 'x', heading: 'tag line' }), null)
})

test('a block id embeds its paragraph, its list item, or the block above a marker line', () => {
  assert.equal(embeddedSection(note, { note: 'Plan', block: 'goal' }), 'Ship it.')
  assert.equal(embeddedSection(note, { note: 'Plan', block: 'second' }), '- two')
  assert.equal(embeddedSection(note, { note: 'Plan', block: 'table' }), '| a | b |\n|---|---|\n| 1 | 2 |')
  assert.equal(embeddedSection('first line\nsecond line ^p\n', { note: 'x', block: 'p' }), 'first line\nsecond line')
  assert.equal(embeddedSection(note, { note: 'Plan', block: 'nope' }), null)
})
