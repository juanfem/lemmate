// Live preview's smaller rules: which `:::` lines are callouts, when the decorations are
// rebuilt, what a task box may tick, and which addresses a rendered link or image may use.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { EditorSelection, EditorState } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { calloutSpans, livePreview, toggleTaskAt, webHref } from '../src/lib/editor/livePreview.ts'
import { noteSyntax } from '../src/lib/editor/syntax.ts'
import { imageSrc } from '../src/lib/linktarget.ts'

function spans(src: string, skipTo = 0) {
  const lines = src.split('\n')
  return calloutSpans((n) => lines[n - 1]!, lines.length, skipTo)
}

test('a callout runs from its fence to the matching closing one', () => {
  const src = '::: {.callout-tip title="Hey"}\ntext\n::: {.inner}\nx\n:::\nmore\n:::\nafter'
  assert.deepEqual(spans(src), [{ open: 1, close: 7, title: 'Hey' }])
})

test('a `:::` inside a code fence or the front matter is not a callout', () => {
  const src = '---\nx: ::: {.callout-note}\n---\n```\n::: {.callout-note}\n```\n~~~~\n:::\n~~~~\n::: {.callout-warning}\nbody\n:::'
  assert.deepEqual(spans(src, 3), [{ open: 10, close: 12, title: 'warning' }])
  // A callout's closing fence inside code does not close it.
  assert.deepEqual(spans('::: {.callout-note}\n```\n:::\n```\n:::'), [{ open: 1, close: 5, title: 'note' }])
})

test('an unclosed callout styles nothing', () => {
  assert.deepEqual(spans('::: {.callout-note}\nthe rest\nof the note'), [])
})

function previewState(doc: string, anchor = 0, readOnly = false) {
  return EditorState.create({
    doc,
    selection: { anchor },
    extensions: [
      markdown({ base: markdownLanguage, extensions: noteSyntax }),
      livePreview({ openLink: () => {}, embedUrl: () => undefined }),
      EditorState.readOnly.of(readOnly),
      EditorState.allowMultipleSelections.of(true),
    ],
  })
}
const decos = (s: EditorState) => s.facet(EditorView.decorations).filter((d) => typeof d !== 'function')

test('moving along a line keeps the decorations; moving to another line rebuilds them', () => {
  const doc = 'some **bold** text\n\nand *more* here'
  const state = previewState(doc, 0)
  const along = state.update({ selection: { anchor: 4 } }).state
  assert.deepEqual(decos(along), decos(state))
  assert.ok(decos(along).every((d, i) => d === decos(state)[i]), 'not rebuilt')
  const away = along.update({ selection: { anchor: doc.length } }).state
  assert.ok(decos(away).some((d, i) => d !== decos(along)[i]), 'rebuilt')
  // Several ranges count range by range.
  const multi = state.update({ selection: EditorSelection.create([EditorSelection.cursor(1), EditorSelection.cursor(doc.length)]) }).state
  assert.ok(decos(multi).some((d, i) => d !== decos(state)[i]))
})

test('a task box ticks its own marker, in a quote too, and never in a read-only view', () => {
  const doc = '> - [ ] quoted\n1. [x] numbered'
  const s = previewState(doc)
  const quoted = doc.indexOf('[ ]')
  assert.equal(s.update(toggleTaskAt(s, quoted)!).state.doc.toString(), '> - [x] quoted\n1. [x] numbered')
  const numbered = doc.indexOf('[x]')
  assert.equal(s.update(toggleTaskAt(s, numbered)!).state.doc.toString(), '> - [ ] quoted\n1. [ ] numbered')
  assert.equal(toggleTaskAt(s, 0), null)
  assert.equal(toggleTaskAt(previewState(doc, 0, true), quoted), null)
})

test('a rendered table link carries only a web address', () => {
  assert.equal(webHref('https://example.com/a'), 'https://example.com/a')
  assert.equal(webHref('javascript:alert(1)'), undefined)
  assert.equal(webHref('JavaScript:alert(1)'), undefined)
  assert.equal(webHref('docs/file.pdf'), undefined)
  assert.equal(webHref('ftp://example.com'), undefined)
})

test('an image may load our attachments and other sites, and nothing else of ours', () => {
  const origin = 'https://notes.example'
  const att = '/api/v1/vaults/01ABC/attachments/deadbeef'
  assert.equal(imageSrc(att, origin), `${origin}${att}`)
  assert.equal(imageSrc(`${origin}${att}`, origin), `${origin}${att}`)
  assert.equal(imageSrc('https://elsewhere.example/pic.png', origin), 'https://elsewhere.example/pic.png')
  assert.equal(imageSrc('data:image/png;base64,AAAA', origin), 'data:image/png;base64,AAAA')
  for (const bad of [
    '/api/v1/vaults/01ABC/daily/2026-10-05',
    `${origin}/api/v1/vaults/01ABC/notes`,
    '/api/v1/vaults/01ABC/attachments/x/../../daily/2026-10-05',
    '/api/v1/vaults/01ABC/notes/01X/render',
    '/auth/logout',
    'pic.png',
    'javascript:alert(1)',
    'data:text/html,<b>x</b>',
  ]) {
    assert.equal(imageSrc(bad, origin), undefined, bad)
  }
})
