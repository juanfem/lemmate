import { test } from 'node:test'
import assert from 'node:assert/strict'
import { renderReturn } from '../src/lib/next.ts'

const V = '01M19Q4FBTHVFTR8NXN9PCWQ8B'
const N = '01M36PH1JHVBQWFAZHB6ATZJPM'
const R = '01M398NHVVT8VC8KH4F8CSJTWS'

test('a sign-in returns only to a render of this site', () => {
  const kept = `/api/v1/vaults/${V}/notes/${N}/render/${R}?format=revealjs`
  assert.equal(renderReturn(`?next=${encodeURIComponent(kept)}`), kept)
  assert.equal(renderReturn(`?next=${encodeURIComponent(`/api/v1/vaults/${V}/notes/${N}/render?format=html`)}`), `/api/v1/vaults/${V}/notes/${N}/render?format=html`)
  for (const bad of ['https://evil.example/', '//evil.example/x', `/api/v1/vaults/${V}/notes/${N}/export`, '/api/v1/auth/logout', `/api/v1/vaults/${V}/notes/${N}/render/../../x`, '']) {
    assert.equal(renderReturn(`?next=${encodeURIComponent(bad)}`), null, bad)
  }
  assert.equal(renderReturn(''), null)
})
