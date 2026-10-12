// The margin outline lists a heading as it reads, not as it is written.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { GFM, parser } from '@lezer/markdown'
import { noteSyntax } from '../src/lib/editor/syntax.ts'
import { headingText, readOutline } from '../src/lib/outline.ts'
import { EditorState } from '@codemirror/state'
import { syntaxTree } from '@codemirror/language'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'

const p = parser.configure([GFM, noteSyntax])
function headings(src: string): string[] {
  const out: string[] = []
  p.parse(src).iterate({
    enter: (n) => {
      if (/^ATXHeading\d$/u.test(n.name)) out.push(headingText(n.node, (a, b) => src.slice(a, b)))
    },
  })
  return out
}

test('inline HTML and comments are left out of a heading', () => {
  assert.deepEqual(headings('# Plain\n'), ['Plain'])
  assert.deepEqual(headings('## <span id="setup">Setup</span> steps\n'), ['Setup steps'])
  assert.deepEqual(headings('### A <!-- note --> B <br/>\n'), ['A B'])
  assert.deepEqual(headings('## Closed ##\n'), ['Closed'])
  assert.deepEqual(headings('## a < b and #tag\n'), ['a < b and #tag'])
})

test('markdown is read, not shown', () => {
  assert.deepEqual(headings('## **Bold** and _em_ and ~~gone~~\n'), ['Bold and em and gone'])
  assert.deepEqual(headings('## Run `cargo test`\n'), ['Run cargo test'])
  assert.deepEqual(headings('## See [the docs](https://x.org "t") and ![alt](a.png)\n'), ['See the docs and alt'])
  assert.deepEqual(headings('## [[Note#Part|Alias]] vs [[Other]]\n'), ['Alias vs Other'])
  assert.deepEqual(headings('## a \\*literal\\* star and <https://x.org>\n'), ['a *literal* star and https://x.org'])
  assert.deepEqual(headings('## Nested **bold _and em_**\n'), ['Nested bold and em'])
  assert.deepEqual(headings('## $E=mc^2$ #tag\n'), ['$E=mc^2$ #tag'])
})

test('the outline reads the whole note, not just what the parser had reached', () => {
  // A fresh state parses only its first stretch; the rest waits for the editor's background
  // parser, which never runs here — as it may not, in a note longer than the screen, there.
  const filler = 'Some words in a paragraph that is not a heading at all.\n\n'.repeat(4000)
  const doc = `# First\n\n${filler}## Last #late\n`
  const state = EditorState.create({ doc, extensions: markdown({ base: markdownLanguage, extensions: noteSyntax }) })
  assert.ok(syntaxTree(state).length < doc.length, 'the parse should still be partial')
  let outline = readOutline(state, 5)
  for (let i = 0; !outline.complete && i < 1000; i++) outline = readOutline(state, 5)
  assert.ok(outline.complete)
  assert.deepEqual(
    outline.items.map((h) => [h.level, h.text, h.pos]),
    [
      [1, 'First', 0],
      [2, 'Last #late', doc.lastIndexOf('## Last')],
    ],
  )
  assert.deepEqual(outline.tags, ['late'])
})
