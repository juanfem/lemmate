import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { addDays, dayOf, formatDate, parseIso, pathFor, type DailySettings } from '../src/lib/daily.ts'

const corpus = join(dirname(fileURLToPath(import.meta.url)), '..', '..', 'corpus')
const none: DailySettings = { folder: '', format: '', template: '' }

test('formats agree with Moment and with the Rust formatter (corpus/daily-formats.json)', () => {
  const cases = JSON.parse(readFileSync(join(corpus, 'daily-formats.json'), 'utf8')) as [string, string, string][]
  assert.ok(cases.length > 0)
  for (const [format, day, expected] of cases) {
    const d = parseIso(day)
    assert.ok(d, day)
    assert.equal(formatDate(format, d), expected, `${format} on ${day}`)
  }
})

test('defaults, the vault root, and nested formats', () => {
  const d = { year: 2026, month: 9, day: 25 }
  assert.equal(pathFor(none, d), 'Daily/2026-09-25.md')
  assert.equal(pathFor({ ...none, folder: '/' }, d), '2026-09-25.md')
  assert.equal(pathFor({ ...none, folder: '/Journal/', format: 'YYYY/MM/DD dddd' }, d), 'Journal/2026/09/25 Friday.md')
})

test('a path is read back to its day, and only a path the settings would write', () => {
  const formats = ['YYYY-MM-DD', 'DD.MM.YYYY', 'YYYY/MM/YYYY-MM-DD dddd', 'dddd, MMMM Do YYYY', 'ddd D MMM YY [notes]']
  for (const format of formats) {
    const s = { ...none, folder: 'Journal', format }
    let d = { year: 2025, month: 12, day: 28 }
    for (let i = 0; i < 40; i++, d = addDays(d, 1)) {
      if (format.includes('YY') && !format.includes('YYYY')) continue // two-digit years: not recoverable
      assert.deepEqual(dayOf(s, pathFor(s, d)), d, `${format}: ${pathFor(s, d)}`)
    }
  }
  assert.equal(dayOf(none, 'Daily/2026-02-30.md'), null, 'no such day')
  assert.equal(dayOf(none, 'Daily/2026-09-25 extra.md'), null)
  assert.equal(dayOf(none, 'Other/2026-09-25.md'), null, 'wrong folder')
  assert.equal(dayOf({ ...none, format: 'YYYY-MM-DD dddd' }, 'Daily/2026-09-25 Monday.md'), null, 'wrong weekday')
})

test('parseIso is strict', () => {
  assert.deepEqual(parseIso('2024-02-29'), { year: 2024, month: 2, day: 29 })
  assert.equal(parseIso('2026-02-29'), null)
  assert.equal(parseIso('2026-9-25'), null)
})
