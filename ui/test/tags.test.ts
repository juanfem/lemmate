// The Tags pane draws whatever this returns, so the shape is the feature: which rows exist,
// what each one is called, and where a count is honest about being missing.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { buildTagTree, tagAncestors, type TagNode } from '../src/lib/tags.ts'

/** The tree as `name(count)` lines, indented — close to what the pane actually draws. */
function drawn(nodes: TagNode[], depth = 0): string[] {
  return nodes.flatMap((n) => [
    `${'  '.repeat(depth)}${n.name}(${n.count ?? '-'})`,
    ...drawn(n.children, depth + 1),
  ])
}

test('a flat listing is a flat tree', () => {
  const tree = buildTagTree([
    { tag: 'work', count: 2 },
    { tag: 'admin', count: 1 },
  ])
  assert.deepEqual(drawn(tree), ['admin(1)', 'work(2)'])
})

test('nesting becomes depth, and a row is named by its last segment alone', () => {
  const tree = buildTagTree([
    { tag: 'projects', count: 4 },
    { tag: 'projects/alpha', count: 3 },
    { tag: 'projects/alpha/deep', count: 1 },
    { tag: 'projects/beta', count: 1 },
    { tag: 'zzz', count: 1 },
  ])
  assert.deepEqual(drawn(tree), [
    'projects(4)',
    '  alpha(3)',
    '    deep(1)',
    '  beta(1)',
    'zzz(1)',
  ])
  assert.equal(tree[0]?.children[0]?.tag, 'projects/alpha', 'the whole tag is what a click asks for')
})

test('a branch point the listing skipped is drawn, without a count it does not have', () => {
  // What an older server answers: literal tags only, no prefixes.
  const tree = buildTagTree([
    { tag: 'projects/alpha', count: 3 },
    { tag: 'projects/beta', count: 1 },
  ])
  assert.deepEqual(drawn(tree), ['projects(-)', '  alpha(3)', '  beta(1)'])
})

test('siblings are alphabetical however the listing arrived', () => {
  const tree = buildTagTree([
    { tag: 'p/z', count: 1 },
    { tag: 'p/a', count: 1 },
    { tag: 'p', count: 2 },
    { tag: 'b', count: 1 },
  ])
  assert.deepEqual(drawn(tree), ['b(1)', 'p(2)', '  a(1)', '  z(1)'])
})

test('nothing tagged, nothing drawn', () => {
  assert.deepEqual(buildTagTree([]), [])
  assert.deepEqual(buildTagTree([{ tag: '', count: 0 }]), [], 'and no row for a nameless tag')
})

test('tagAncestors names the branch points above a tag, not the tag', () => {
  assert.deepEqual(tagAncestors('a/b/c'), ['a', 'a/b'])
  assert.deepEqual(tagAncestors('a'), [])
})
