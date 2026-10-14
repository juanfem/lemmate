// The spell checker skips what is not prose: addresses, note links, tags and code — and only
// those, so a link's label and the text around them are still checked.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { EditorState } from '@codemirror/state'
import { ensureSyntaxTree } from '@codemirror/language'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { noteSyntax } from '../src/lib/editor/syntax.ts'
import { uncheckedSpans } from '../src/lib/editor/spelling.ts'

function skipped(doc: string): string[] {
  const state = EditorState.create({ doc, extensions: [markdown({ base: markdownLanguage, extensions: noteSyntax })] })
  ensureSyntaxTree(state, doc.length, 5000)
  return uncheckedSpans(state).map((s) => doc.slice(s.from, s.to))
}

test('a link skips its address, not its label', () => {
  assert.deepEqual(skipped('See [the speling](https://exampel.org/pgae) here.'), ['https://exampel.org/pgae'])
})

test('bare and angle-bracketed addresses are skipped', () => {
  assert.deepEqual(skipped('Go to https://exampel.org/x now'), ['https://exampel.org/x'])
  assert.deepEqual(skipped('Go to <https://exampel.org/x> now'), ['<https://exampel.org/x>'])
})

test('note links, embeds and tags are skipped whole', () => {
  assert.deepEqual(skipped('A [[Notte nmae|alais]] and ![[Imgae.png]] with #tgas.'), ['[[Notte nmae|alais]]', '![[Imgae.png]]', '#tgas'])
})

test('inline code and code blocks are skipped', () => {
  assert.deepEqual(skipped('Run `cargo fmt` frist.'), ['`cargo fmt`'])
  assert.deepEqual(skipped('Text\n\n```rust\nlet xs = vec![];\n```\n\nmore'), ['```rust\nlet xs = vec![];\n```'])
  assert.deepEqual(skipped('Text\n\n    indented cdoe\n\nmore'), ['indented cdoe'])
})

test('plain prose is all checked', () => {
  assert.deepEqual(skipped('# A hedaing\n\nSome *emphasised* txet, a list:\n\n- itme'), [])
})
