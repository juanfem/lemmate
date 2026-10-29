// Folding: an HTML comment folds whole, the arrows sit on headings and comments, and the
// palette folds every comment or every section.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { EditorState } from '@codemirror/state'
import { codeFolding, foldable, foldedRanges } from '@codemirror/language'
import { markdown, markdownLanguage } from '@codemirror/lang-markdown'
import { noteSyntax } from '../src/lib/editor/syntax.ts'
import { commentFolding, arrowsIn, commentRanges, foldCommentsEffects, foldSectionsEffects, unfoldCommentsEffects } from '../src/lib/editor/fold.ts'

const make = (doc: string) =>
  EditorState.create({ doc, extensions: [markdown({ base: markdownLanguage, extensions: noteSyntax }), codeFolding(), commentFolding] })

const hidden = (s: EditorState, r: { from: number; to: number }) => s.sliceDoc(r.from, r.to)

function foldedText(s: EditorState): string[] {
  const out: string[] = []
  foldedRanges(s).between(0, s.doc.length, (from, to) => void out.push(s.sliceDoc(from, to)))
  return out
}

test('a comment hides what lies between its markers, block or inline', () => {
  const s = make('Text <!-- inline --> here.\n\n<!--\nblock\ncomment\n--> tail\n\n```html\n<!-- code -->\n```\n')
  assert.deepEqual(
    commentRanges(s).map((r) => hidden(s, r)),
    [' inline ', '\nblock\ncomment\n'],
  )
})

test('a comment left open runs to the end of what the parser read as it', () => {
  const s = make('<!-- never\nclosed\n')
  assert.deepEqual(
    commentRanges(s).map((r) => hidden(s, r)),
    [' never\nclosed\n'],
  )
})

test('a comment folds from its opening line, and only one that spans lines', () => {
  const s = make('<!-- a\nb -->\n\nx <!-- one line -->\n')
  const first = s.doc.line(1)
  assert.equal(hidden(s, foldable(s, first.from, first.to)!), ' a\nb ')
  const fourth = s.doc.line(4)
  assert.equal(foldable(s, fourth.from, fourth.to), null)
})

test('the arrows go beside headings and comments, not every paragraph that folds', () => {
  const doc = '# Title\n\nA paragraph\nover two lines.\n\n<!-- a\nb -->\n\n| a |\n|---|\n| b |\n\n## Sub\n\nend\n'
  const s = make(doc)
  assert.deepEqual(
    arrowsIn(s, 0, doc.length).map((a) => s.doc.lineAt(a.at).number),
    [1, 6, 13],
  )
  const folded = s.update({ effects: foldSectionsEffects(s) }).state
  assert.deepEqual(
    arrowsIn(folded, 0, doc.length).map((a) => a.open),
    [false, true, false],
  )
})

test('folding every comment, then unfolding them, leaves a folded section alone', () => {
  let s = make('# A\n\n<!-- one -->\n\ntext\n\n# B\n\n<!-- two\nlines -->\n')
  s = s.update({ effects: foldSectionsEffects(s).slice(0, 1) }).state
  assert.deepEqual(foldedText(s), ['\n\n<!-- one -->\n\ntext'])
  s = s.update({ effects: foldCommentsEffects(s) }).state
  assert.equal(foldedText(s).length, 3)
  assert.equal(foldCommentsEffects(s).length, 0, 'nothing left to fold')
  s = s.update({ effects: unfoldCommentsEffects(s) }).state
  assert.deepEqual(foldedText(s), ['\n\n<!-- one -->\n\ntext'])
})

test('folding sections folds every heading, nested ones too, and not a heading in code', () => {
  const s = make('# A\n\n## A.1\n\none\n\n## A.2\n\ntwo\n\n```\n# not\n\nreally\n```\n# B\n\nthree\n')
  const t = s.update({ effects: foldSectionsEffects(s) }).state
  assert.equal(foldedText(t).length, 4)
  assert.ok(foldedText(t)[0]!.includes('## A.2'))
})
