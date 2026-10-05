// The margin outline lists a heading as it reads, not as it is written.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { GFM, parser } from '@lezer/markdown'
import { noteSyntax } from '../src/lib/editor/syntax.ts'
import { headingText } from '../src/lib/outline.ts'

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
