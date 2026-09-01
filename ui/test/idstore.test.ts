import { test } from 'node:test'
import assert from 'node:assert/strict'
import { parseIds } from '../src/lib/idstore.ts'

test('parseIds tolerates every shape a stored value can take', () => {
  assert.deepEqual(parseIds(null), [])
  assert.deepEqual(parseIds(''), [])
  assert.deepEqual(parseIds('{'), [], 'a truncated write must not break start-up')
  assert.deepEqual(parseIds('"nope"'), [], 'valid JSON, wrong shape')
  assert.deepEqual(parseIds('{"a":1}'), [])
})

test('parseIds keeps the ids and drops everything else', () => {
  assert.deepEqual(parseIds('["01A","01B"]'), ['01A', '01B'])
  assert.deepEqual(parseIds('["01A",null,7,"",{"id":"x"},"01B"]'), ['01A', '01B'])
})
