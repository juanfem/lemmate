// The browser sync layer against a real lemmate-server: create a note from "the UI" (Y.Doc ops
// over the frame protocol), see the server index it, and see `lemmate sync` project it to disk.
// Skipped unless LEMMATE_SERVER_BIN and LEMMATE_CLI_BIN point at built binaries.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { spawn, spawnSync } from 'node:child_process'
import { mkdtempSync, readFileSync, existsSync, readdirSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import * as Y from 'yjs'
import { SyncClient } from '../src/lib/sync.ts'
import { ulid } from '../src/lib/ulid.ts'

const SERVER = process.env.LEMMATE_SERVER_BIN
const CLI = process.env.LEMMATE_CLI_BIN
const PORT = 18095

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

async function waitFor(pred: () => Promise<boolean> | boolean, ms = 10_000): Promise<void> {
  const deadline = Date.now() + ms
  while (Date.now() < deadline) {
    if (await pred()) return
    await sleep(100)
  }
  throw new Error('timed out')
}

test('browser client creates and edits notes through the relay', { skip: !SERVER || !CLI }, async () => {
  const data = mkdtempSync(join(tmpdir(), 'notes-web-'))
  const server = spawn(SERVER!, ['--bind', `127.0.0.1:${PORT}`, '--data-dir', data, '--no-auth'], { stdio: 'ignore' })
  const clients: SyncClient[] = []
  try {
    await waitFor(async () => {
      try {
        return (await fetch(`http://127.0.0.1:${PORT}/healthz`)).ok
      } catch {
        return false
      }
    })

    const vaultId = ulid()
    const client = new SyncClient(`ws://127.0.0.1:${PORT}/ws`)
    clients.push(client)
    const vault = new Y.Doc()
    client.open(`vault:${vaultId}`, vault)
    await waitFor(() => client.isSynced(`vault:${vaultId}`))

    // Create a note exactly as VaultSession.createNote does.
    const noteId = ulid()
    const note = new Y.Doc()
    client.open(noteId, note)
    note.getText('content').insert(0, `---\nid: ${noteId}\n---\n# Hello from the browser\n\n#web tag and [[Other]]\n`)
    vault.getMap<string>('notes').set(noteId, 'Inbox/Hello.md')

    // The server derives its relational view from the stream.
    const api = `http://127.0.0.1:${PORT}/api/v1/vaults/${vaultId}`
    await waitFor(async () => {
      const notes = (await (await fetch(`${api}/notes`)).json()) as { id: string; path: string; title: string | null }[]
      return notes.length === 1 && notes[0]!.title === 'Hello from the browser'
    })
    const hits = (await (await fetch(`${api}/search?q=browser`)).json()) as { note_id: string }[]
    assert.equal(hits[0]?.note_id, noteId)
    const tags = (await (await fetch(`${api}/tags`)).json()) as { tag: string }[]
    assert.deepEqual(
      tags.map((t) => t.tag),
      ['web'],
    )

    // A projection client sees the file on disk, with the browser's edit merged in.
    const dir = mkdtempSync(join(tmpdir(), 'notes-proj-'))
    const first = spawnSync(CLI!, ['sync', '--vault', dir, '--server', `http://127.0.0.1:${PORT}`, '--vault-id', vaultId, '--once'])
    assert.equal(first.status, 0, first.stderr.toString())
    const text = readFileSync(join(dir, 'Inbox/Hello.md'), 'utf8')
    assert.ok(text.includes('# Hello from the browser'), text)

    // Live edit from the browser reaches the next projection run.
    note.getText('content').insert(note.getText('content').length, 'appended live\n')
    await sleep(300)
    const second = spawnSync(CLI!, ['sync', '--vault', dir, '--server', `http://127.0.0.1:${PORT}`, '--once'])
    assert.equal(second.status, 0, second.stderr.toString())
    assert.ok(readFileSync(join(dir, 'Inbox/Hello.md'), 'utf8').endsWith('appended live\n'))

    // And a second browser client converges on both docs.
    const other = new SyncClient(`ws://127.0.0.1:${PORT}/ws`)
    clients.push(other)
    const vault2 = new Y.Doc()
    const note2 = new Y.Doc()
    other.open(`vault:${vaultId}`, vault2)
    other.open(noteId, note2)
    await waitFor(() => other.isSynced(noteId) && note2.getText('content').toString() === note.getText('content').toString())
    assert.equal(vault2.getMap<string>('notes').get(noteId), 'Inbox/Hello.md')

    // Rename from the browser moves the file for the projection client.
    vault.getMap<string>('notes').set(noteId, 'Hello-renamed.md')
    await sleep(300)
    spawnSync(CLI!, ['sync', '--vault', dir, '--server', `http://127.0.0.1:${PORT}`, '--once'])
    assert.ok(existsSync(join(dir, 'Hello-renamed.md')), readdirSync(dir).join(','))
    assert.ok(!existsSync(join(dir, 'Inbox/Hello.md')))

  } finally {
    for (const c of clients) c.destroy()
    server.kill()
  }
})
