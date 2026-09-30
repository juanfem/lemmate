// Following a markdown link or a bare address: where `linkTarget` sends it, and which spans the
// live preview marks as links for its click handler.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { EditorState } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { linkTarget } from '../src/lib/linktarget.ts'
import { bareHref, livePreview } from '../src/lib/editor/livePreview.ts'
import { noteSyntax } from '../src/lib/editor/syntax.ts'

test('an address with a scheme goes to the web', () => {
  assert.deepEqual(linkTarget('a/b.md', 'https://example.com/x?y#z'), { kind: 'web', url: 'https://example.com/x?y#z' })
  assert.deepEqual(linkTarget('b.md', 'mailto:me@example.com'), { kind: 'web', url: 'mailto:me@example.com' })
  assert.deepEqual(linkTarget('b.md', '<https://example.com/a b>'), { kind: 'web', url: 'https://example.com/a b' })
})

test('scripts and data are not followed', () => {
  assert.equal(linkTarget('b.md', 'javascript:alert(1)'), undefined)
  assert.equal(linkTarget('b.md', ' JavaScript:alert(1)'), undefined)
  assert.equal(linkTarget('b.md', 'data:text/html,<b>x</b>'), undefined)
  assert.equal(linkTarget('b.md', ''), undefined)
  assert.equal(linkTarget('b.md', '#heading'), undefined)
})

test('a relative path is relative to the note, a leading slash to the vault', () => {
  assert.deepEqual(linkTarget('Projects/plan.md', 'spec.pdf'), { kind: 'file', path: 'Projects/spec.pdf' })
  assert.deepEqual(linkTarget('Projects/plan.md', './attachments/My%20file.pdf'), { kind: 'file', path: 'Projects/attachments/My file.pdf' })
  assert.deepEqual(linkTarget('Projects/plan.md', '../Other/note.md#part'), { kind: 'note', path: 'Other/note.md' })
  assert.deepEqual(linkTarget('Projects/plan.md', '/attachments/x.png'), { kind: 'file', path: 'attachments/x.png' })
  assert.deepEqual(linkTarget('plan.md', '../../x.qmd'), { kind: 'note', path: 'x.qmd' })
  assert.deepEqual(linkTarget('plan.md', '<My Note>'), { kind: 'note', path: 'My Note' })
  assert.deepEqual(linkTarget('plan.md', '100%.txt'), { kind: 'file', path: '100%.txt' })
})

test('a bare address gets the scheme GFM leaves off', () => {
  assert.equal(bareHref('https://example.com'), 'https://example.com')
  assert.equal(bareHref('www.example.com/a'), 'http://www.example.com/a')
  assert.equal(bareHref('me@example.com'), 'mailto:me@example.com')
})

/** The `data-href` of every link span the preview draws over `doc`, with the text it covers. */
function links(doc: string, cursor = doc.length) {
  const state = EditorState.create({
    doc,
    selection: { anchor: cursor },
    extensions: [
      markdown({ base: markdownLanguage, extensions: noteSyntax }),
      livePreview({ openLink: () => {}, embedUrl: () => undefined, openUrl: () => {} }),
    ],
  })
  const out: { text: string; href: string; rendered: boolean }[] = []
  for (const source of state.facet(EditorView.decorations)) {
    const set = typeof source === 'function' ? null : source
    set?.between(0, doc.length, (from, to, deco) => {
      const attrs = deco.spec.attributes as Record<string, string> | undefined
      if (attrs?.['data-href'] !== undefined) {
        out.push({ text: doc.slice(from, to), href: attrs['data-href'], rendered: /cm-link-rendered/u.test(deco.spec.class) })
      }
    })
  }
  return out
}

test('link text, autolinks and bare addresses are marked with where they go', () => {
  const doc = 'See [the *docs*](https://example.com/docs "t"), <https://a.example> and www.b.example.\n\nend'
  assert.deepEqual(links(doc), [
    { text: 'the *docs*', href: 'https://example.com/docs', rendered: true },
    { text: 'https://a.example', href: 'https://a.example', rendered: true },
    { text: 'www.b.example', href: 'http://www.b.example', rendered: true },
  ])
})

test('on the line being edited, a link is marked but not rendered', () => {
  const doc = '[x](y.pdf) and https://c.example\n\nend'
  assert.deepEqual(links(doc, 1), [
    { text: 'x', href: 'y.pdf', rendered: false },
    { text: 'https://c.example', href: 'https://c.example', rendered: false },
  ])
})
