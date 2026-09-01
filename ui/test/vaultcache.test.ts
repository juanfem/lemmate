import { test } from 'node:test'
import assert from 'node:assert/strict'
import { parse } from '../src/lib/vaultcache.ts'

test('parse survives everything a stored value can be', () => {
  assert.deepEqual(parse(null), [], 'nothing stored yet')
  assert.deepEqual(parse(''), [])
  assert.deepEqual(parse('not json {['), [], 'a truncated write must not break start-up')
  assert.deepEqual(parse('"a string"'), [], 'valid JSON of the wrong shape')
  assert.deepEqual(parse('{"id":"x"}'), [], 'an object where an array belongs')
})

test('parse keeps the ids and drops the rubbish around them', () => {
  assert.deepEqual(parse('["01ABC","01DEF"]'), ['01ABC', '01DEF'])
  assert.deepEqual(parse('["01ABC",null,42,"",{"nope":1},"01DEF"]'), ['01ABC', '01DEF'])
})

test('parse still reads the shape the first version wrote', () => {
  // Anyone who ran a build between the two commits has `{ id, notes }` objects in their
  // localStorage; forgetting their vault list offline would be a poor way to find out.
  assert.deepEqual(parse('[{"id":"01ABC","notes":3},{"id":"01DEF","notes":0}]'), ['01ABC', '01DEF'])
})
