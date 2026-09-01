import { test } from 'node:test'
import assert from 'node:assert/strict'
import { parseIds, parseMap } from '../src/lib/idstore.ts'

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

test('parseMap reads the id-to-version record, and survives junk', () => {
  assert.deepEqual([...parseMap(null)], [])
  assert.deepEqual([...parseMap('[]')], [], 'an array is the old set shape, not a map')
  assert.deepEqual([...parseMap('nope')], [])
  assert.deepEqual([...parseMap('{"01A":"2026-09-01T00:00:00Z"}')], [['01A', '2026-09-01T00:00:00Z']])
  assert.deepEqual([...parseMap('{"01A":"v1","01B":7,"01C":null,"":"v2"}')], [['01A', 'v1']],
    'only string versions under non-empty ids')
})
