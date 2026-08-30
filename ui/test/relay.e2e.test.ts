// The local relay (`lemmate sync --serve`): a UI client talks to the engine on loopback, the
// engine projects to disk and forwards to the real server — and keeps working while the server
// is down, pushing the backlog when it returns.

import { test } from 'node:test'
import assert from 'node:assert/strict'
import { spawn, type ChildProcess } from 'node:child_process'
import { mkdtempSync, readFileSync, existsSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import * as Y from 'yjs'
import { SyncClient } from '../src/lib/sync.ts'
import { ulid } from '../src/lib/ulid.ts'

const SERVER = process.env.LEMMATE_SERVER_BIN
const CLI = process.env.LEMMATE_CLI_BIN
const SERVER_PORT = 18097
const RELAY_PORT = 18098

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))
async function waitFor(pred: () => Promise<boolean> | boolean, ms = 10_000, what = 'condition'): Promise<void> {
  const deadline = Date.now() + ms
  while (Date.now() < deadline) {
    if (await pred()) return
    await sleep(100)
  }
  throw new Error(`timed out waiting for ${what}`)
}
async function up(port: number): Promise<boolean> {
  try {
    return (await fetch(`http://127.0.0.1:${port}/healthz`)).ok
  } catch {
    return false
  }
}
async function json<T>(url: string): Promise<T> {
  return (await (await fetch(url)).json()) as T
}

test('relay serves a UI offline and forwards to the server', { skip: !SERVER || !CLI }, async () => {
  const data = mkdtempSync(join(tmpdir(), 'notes-relay-srv-'))
  const vaultDir = mkdtempSync(join(tmpdir(), 'notes-relay-vault-'))
  const startServer = () => spawn(SERVER!, ['--bind', `127.0.0.1:${SERVER_PORT}`, '--data-dir', data, '--no-auth'], { stdio: 'ignore' })
  let server: ChildProcess = startServer()
  let relay: ChildProcess | undefined
  const clients: SyncClient[] = []
  try {
    await waitFor(() => up(SERVER_PORT), 10_000, 'server')
    relay = spawn(CLI!, ['sync', '--vault', vaultDir, '--server', `http://127.0.0.1:${SERVER_PORT}`, '--serve', `127.0.0.1:${RELAY_PORT}`], {
      stdio: 'ignore',
    })
    await waitFor(() => up(RELAY_PORT), 10_000, 'relay')
    const [vault] = await json<{ id: string; notes: number }[]>(`http://127.0.0.1:${RELAY_PORT}/api/v1/vaults`)
    assert.ok(vault, 'relay lists its vault')
    const vaultId = vault!.id

    // A UI client on the relay creates a note.
    const client = new SyncClient(`ws://127.0.0.1:${RELAY_PORT}/ws`)
    clients.push(client)
    const vdoc = new Y.Doc()
    client.open(`vault:${vaultId}`, vdoc)
    await waitFor(() => client.isSynced(`vault:${vaultId}`), 10_000, 'vault handshake')
    const noteId = ulid()
    const note = new Y.Doc()
    client.open(noteId, note)
    note.getText('content').insert(0, `---\nid: ${noteId}\n---\n# Via relay\n\nfirst\n`)
    vdoc.getMap<string>('notes').set(noteId, 'Relay.md')

    // ... which the engine projects to disk and the server indexes.
    await waitFor(() => existsSync(join(vaultDir, 'Relay.md')), 10_000, 'projection')
    const serverApi = `http://127.0.0.1:${SERVER_PORT}/api/v1/vaults/${vaultId}`
    await waitFor(async () => (await json<{ path: string }[]>(`${serverApi}/notes`)).some((n) => n.path === 'Relay.md'), 10_000, 'server index')
    // The relay's own API answers from the local store.
    await waitFor(async () => (await json<{ note_id: string }[]>(`http://127.0.0.1:${RELAY_PORT}/api/v1/vaults/${vaultId}/search?q=relay`)).length === 1, 10_000, 'local search')

    // A file pasted into the editor: PUT to the relay files it under attachments/, and once a
    // note references it the engine records it in the vault doc and uploads it.
    const png = new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10, 1, 2, 3])
    const { blake3Hex } = await import('../src/lib/blake3.ts')
    const hash = await blake3Hex(png)
    const put = await fetch(`http://127.0.0.1:${RELAY_PORT}/api/v1/vaults/${vaultId}/attachments/${hash}`, {
      method: 'PUT',
      headers: { 'content-type': 'image/png', 'x-filename': 'shot.png' },
      body: png,
    })
    assert.equal(put.status, 200)
    const stored = (await put.json()) as { path: string; hash: string }
    assert.equal(stored.path, 'attachments/shot.png')
    assert.ok(existsSync(join(vaultDir, 'attachments/shot.png')))
    note.getText('content').insert(note.getText('content').length, '![[shot.png]]\n')
    await waitFor(() => vdoc.getMap<string>('attachments').get('attachments/shot.png') === hash, 10_000, 'attachment entry')
    await waitFor(async () => (await fetch(`${serverApi}/attachments/${hash}`)).ok, 10_000, 'server blob')

    // Server goes away. The UI keeps editing through the relay; disk keeps up.
    server.kill()
    await waitFor(async () => !(await up(SERVER_PORT)), 10_000, 'server down')
    note.getText('content').insert(note.getText('content').length, 'offline edit\n')
    await waitFor(() => readFileSync(join(vaultDir, 'Relay.md'), 'utf8').includes('offline edit'), 10_000, 'offline projection')
    // A second UI client on the relay sees the change without any server.
    const other = new SyncClient(`ws://127.0.0.1:${RELAY_PORT}/ws`)
    clients.push(other)
    const note2 = new Y.Doc()
    other.open(noteId, note2)
    await waitFor(() => note2.getText('content').toString().includes('offline edit'), 10_000, 'second client')

    // Server returns: the relay reconnects and pushes the backlog.
    server = startServer()
    await waitFor(() => up(SERVER_PORT), 10_000, 'server back')
    await waitFor(
      async () => {
        const notes = await json<{ id: string }[]>(`${serverApi}/notes`)
        if (!notes.some((n) => n.id === noteId)) return false
        const body = await json<{ content: string }>(`${serverApi}/notes/${noteId}`)
        return body.content.includes('offline edit')
      },
      20_000,
      'backlog pushed',
    )
  } finally {
    for (const c of clients) c.destroy()
    relay?.kill()
    server.kill()
  }
})
