// SyncClient against a stand-in server that behaves as lemmate-server does on the wire: one
// connection's frames handled in order, a SyncStep1 answered with SyncStep2 then SyncStep1.
// The stand-in can hold frames back, which is how a test sees what the client concludes
// before the server has caught up.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import * as Y from 'yjs'
import * as syncProtocol from 'y-protocols/sync'
import * as awarenessProtocol from 'y-protocols/awareness'
import * as encoding from 'lib0/encoding'
import * as decoding from 'lib0/decoding'
import { decodeFrame, encodeFrame } from '../src/lib/frames.ts'

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

class FakeServer {
  docs = new Map<string, Y.Doc>()
  awareness = new Map<string, awarenessProtocol.Awareness>()
  inbox: [FakeSocket, Uint8Array][] = []
  sockets: FakeSocket[] = []
  /** Handle frames as they arrive; when false, they wait for `step()`. */
  auto = true

  doc(id: string): Y.Doc {
    let d = this.docs.get(id)
    if (!d) this.docs.set(id, (d = new Y.Doc()))
    return d
  }

  receive(sock: FakeSocket, bytes: Uint8Array) {
    this.inbox.push([sock, bytes])
    if (this.auto) this.drain()
  }

  /** Handle the oldest waiting frame; false when there is none. */
  step(): boolean {
    const next = this.inbox.shift()
    if (!next) return false
    const [sock, bytes] = next
    if (sock.readyState !== 1) return true
    const { docId, payload } = decodeFrame(bytes)
    const dec = decoding.createDecoder(payload)
    const kind = decoding.readVarUint(dec)
    const doc = this.doc(docId)
    if (kind === 0) {
      const sub = decoding.readVarUint(dec)
      if (sub === syncProtocol.messageYjsSyncStep1) {
        const sv = decoding.readVarUint8Array(dec)
        const step2 = encoding.createEncoder()
        encoding.writeVarUint(step2, 0)
        syncProtocol.writeSyncStep2(step2, doc, sv)
        sock.deliver(encodeFrame(docId, encoding.toUint8Array(step2)))
        const step1 = encoding.createEncoder()
        encoding.writeVarUint(step1, 0)
        syncProtocol.writeSyncStep1(step1, doc)
        sock.deliver(encodeFrame(docId, encoding.toUint8Array(step1)))
      } else {
        Y.applyUpdate(doc, decoding.readVarUint8Array(dec))
      }
    } else if (kind === 1) {
      let a = this.awareness.get(docId)
      if (!a) this.awareness.set(docId, (a = new awarenessProtocol.Awareness(new Y.Doc())))
      awarenessProtocol.applyAwarenessUpdate(a, decoding.readVarUint8Array(dec), 'client')
    }
    return true
  }

  drain() {
    while (this.step());
  }
}

const server = new FakeServer()

class FakeSocket {
  static OPEN = 1
  readyState = 0
  binaryType = ''
  onopen: (() => void) | null = null
  onmessage: ((ev: { data: ArrayBuffer }) => void) | null = null
  onclose: (() => void) | null = null
  onerror: (() => void) | null = null
  constructor(_url: string) {
    server.sockets.push(this)
    setTimeout(() => {
      this.readyState = 1
      this.onopen?.()
    }, 0)
  }
  send(bytes: Uint8Array) {
    if (this.readyState !== 1) throw new Error('send on a closed socket')
    server.receive(this, bytes.slice())
  }
  deliver(bytes: Uint8Array) {
    if (this.readyState === 1) this.onmessage?.({ data: bytes.slice().buffer })
  }
  close() {
    if (this.readyState === 3) return
    this.readyState = 3
    setTimeout(() => this.onclose?.(), 0)
  }
}
;(globalThis as unknown as { WebSocket: unknown }).WebSocket = FakeSocket
const { SyncClient } = await import('../src/lib/sync.ts')

async function until(pred: () => boolean, ms = 3000) {
  const end = Date.now() + ms
  while (!pred()) {
    if (Date.now() > end) throw new Error('timed out')
    await sleep(10)
  }
}

test('acknowledged only once the server has applied our changes, not when it has sent its own', async () => {
  server.auto = false
  const client = new SyncClient('ws://test')
  try {
    const acked: string[] = []
    const synced: string[] = []
    client.onAcked = (id) => acked.push(id)
    client.onSynced = (id) => synced.push(id)
    const doc = new Y.Doc()
    doc.getText('content').insert(0, 'written offline') // already in the doc as it opens
    client.open('n1', doc)
    await until(() => server.inbox.length > 0)
    server.step() // our SyncStep1: the server answers, we reply with our SyncStep2
    assert.deepEqual(synced, ['n1'])
    assert.deepEqual(acked, [], 'the server has not applied our SyncStep2 yet')
    assert.equal(server.doc('n1').getText('content').toString(), '')
    server.drain()
    await until(() => acked.length > 0)
    assert.equal(server.doc('n1').getText('content').toString(), 'written offline')

    // An edit made online: acknowledged after the server has it, and not before.
    acked.length = 0
    doc.getText('content').insert(0, '+')
    await sleep(600) // past the probe's settling delay
    assert.deepEqual(acked, [])
    server.drain()
    await until(() => acked.length > 0)
    assert.equal(server.doc('n1').getText('content').toString(), '+written offline')
  } finally {
    client.destroy()
    server.auto = true
    server.drain()
  }
})

test('closing a doc tells the server our cursor is gone', async () => {
  const client = new SyncClient('ws://test')
  try {
    const doc = new Y.Doc()
    const awareness = client.open('n2', doc)
    awareness.setLocalStateField('user', { name: 'me' })
    await until(() => client.isSynced('n2'))
    const there = server.awareness.get('n2')!
    await until(() => there.getStates().has(doc.clientID))
    client.close('n2')
    await until(() => !there.getStates().has(doc.clientID))
  } finally {
    client.destroy()
  }
})

test('after the server drops the connection every open doc handshakes again', async () => {
  const client = new SyncClient('ws://test')
  try {
    const a = new Y.Doc()
    const b = new Y.Doc()
    client.open('n3', a)
    client.open('n4', b)
    await until(() => client.isSynced('n3') && client.isSynced('n4'))
    // The server closes a connection that fell behind; edits made meanwhile go nowhere.
    server.sockets.at(-1)!.close()
    await until(() => !client.isSynced('n3'))
    a.getText('content').insert(0, 'while away')
    b.getText('content').insert(0, 'also')
    assert.equal(server.doc('n3').getText('content').toString(), '')
    await until(() => client.isSynced('n3') && client.isSynced('n4'), 5000)
    await until(() => server.doc('n3').getText('content').toString() === 'while away')
    assert.equal(server.doc('n4').getText('content').toString(), 'also')
  } finally {
    client.destroy()
  }
})
