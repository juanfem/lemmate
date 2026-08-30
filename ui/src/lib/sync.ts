// One WebSocket per vault, multiplexing every open Y.Doc through the frame protocol. Speaks the
// standard y-protocols sync/awareness messages the server relays (SPEC §7).

import * as Y from 'yjs'
import * as syncProtocol from 'y-protocols/sync'
import * as awarenessProtocol from 'y-protocols/awareness'
import * as encoding from 'lib0/encoding'
import * as decoding from 'lib0/decoding'
import { decodeFrame, encodeFrame } from './frames.ts'

const MSG_SYNC = 0
const MSG_AWARENESS = 1
const MSG_AUTH = 2

export type SyncStatus = 'connecting' | 'online' | 'offline'

interface Entry {
  doc: Y.Doc
  awareness: awarenessProtocol.Awareness
  onUpdate: (update: Uint8Array, origin: unknown) => void
  onAwareness: (changes: { added: number[]; updated: number[]; removed: number[] }, origin: unknown) => void
  synced: boolean
}

export class SyncClient {
  private ws: WebSocket | null = null
  private docs = new Map<string, Entry>()
  private backoff = 1000
  private closed = false
  status: SyncStatus = 'connecting'
  onStatus: (s: SyncStatus) => void = () => {}
  onSynced: (docId: string) => void = () => {}
  /** The server refused a read or write on a doc (SPEC §11.2). */
  onDenied: (docId: string, reason: string) => void = () => {}
  private url: string

  constructor(url: string) {
    this.url = url
    this.connect()
  }

  static wsUrl(): string {
    const proto = location.protocol === 'https:' ? 'wss' : 'ws'
    return `${proto}://${location.host}/ws`
  }

  private connect() {
    if (this.closed) return
    this.setStatus('connecting')
    const ws = new WebSocket(this.url)
    ws.binaryType = 'arraybuffer'
    this.ws = ws
    ws.onopen = () => {
      this.backoff = 1000
      this.setStatus('online')
      for (const [docId, entry] of this.docs) this.handshake(docId, entry)
    }
    ws.onmessage = (ev) => {
      if (ev.data instanceof ArrayBuffer) this.handle(new Uint8Array(ev.data))
    }
    ws.onclose = () => {
      this.ws = null
      this.setStatus('offline')
      for (const e of this.docs.values()) e.synced = false
      if (!this.closed) {
        setTimeout(() => this.connect(), this.backoff)
        this.backoff = Math.min(this.backoff * 2, 30_000)
      }
    }
    ws.onerror = () => ws.close()
  }

  private setStatus(s: SyncStatus) {
    this.status = s
    this.onStatus(s)
  }

  /** Register a doc; it syncs now if online and on every reconnect. */
  open(docId: string, doc: Y.Doc): awarenessProtocol.Awareness {
    const existing = this.docs.get(docId)
    if (existing) return existing.awareness
    const awareness = new awarenessProtocol.Awareness(doc)
    const entry: Entry = {
      doc,
      awareness,
      synced: false,
      onUpdate: (update, origin) => {
        if (origin === this) return
        const enc = encoding.createEncoder()
        encoding.writeVarUint(enc, MSG_SYNC)
        syncProtocol.writeUpdate(enc, update)
        this.send(docId, encoding.toUint8Array(enc))
      },
      onAwareness: ({ added, updated, removed }) => {
        const changed = added.concat(updated, removed)
        const enc = encoding.createEncoder()
        encoding.writeVarUint(enc, MSG_AWARENESS)
        encoding.writeVarUint8Array(enc, awarenessProtocol.encodeAwarenessUpdate(awareness, changed))
        this.send(docId, encoding.toUint8Array(enc))
      },
    }
    doc.on('update', entry.onUpdate)
    awareness.on('update', entry.onAwareness)
    this.docs.set(docId, entry)
    if (this.ws?.readyState === WebSocket.OPEN) this.handshake(docId, entry)
    return awareness
  }

  close(docId: string) {
    const e = this.docs.get(docId)
    if (!e) return
    e.doc.off('update', e.onUpdate)
    e.awareness.off('update', e.onAwareness)
    awarenessProtocol.removeAwarenessStates(e.awareness, [e.doc.clientID], 'close')
    e.awareness.destroy() // stops its keep-alive timer
    this.docs.delete(docId)
  }

  isSynced(docId: string): boolean {
    return this.docs.get(docId)?.synced ?? false
  }

  destroy() {
    this.closed = true
    for (const id of [...this.docs.keys()]) this.close(id)
    this.ws?.close()
  }

  private handshake(docId: string, entry: Entry) {
    const enc = encoding.createEncoder()
    encoding.writeVarUint(enc, MSG_SYNC)
    syncProtocol.writeSyncStep1(enc, entry.doc)
    this.send(docId, encoding.toUint8Array(enc))
    // Push our awareness state right away so cursors show up on the other side.
    const local = entry.awareness.getLocalState()
    if (local) entry.onAwareness({ added: [], updated: [entry.doc.clientID], removed: [] }, this)
  }

  private send(docId: string, payload: Uint8Array) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(encodeFrame(docId, payload))
  }

  private handle(bytes: Uint8Array) {
    let frame
    try {
      frame = decodeFrame(bytes)
    } catch {
      return
    }
    const entry = this.docs.get(frame.docId)
    if (!entry) return
    const decoder = decoding.createDecoder(frame.payload)
    switch (decoding.readVarUint(decoder)) {
      case MSG_SYNC: {
        const enc = encoding.createEncoder()
        encoding.writeVarUint(enc, MSG_SYNC)
        const kind = syncProtocol.readSyncMessage(decoder, enc, entry.doc, this)
        if (encoding.length(enc) > 1) this.send(frame.docId, encoding.toUint8Array(enc))
        // The server answers our SyncStep1 with SyncStep2 (its state) then its own SyncStep1;
        // after we have replied to that, both sides hold the same state.
        if (kind === syncProtocol.messageYjsSyncStep1 && !entry.synced) {
          entry.synced = true
          this.onSynced(frame.docId)
        }
        break
      }
      case MSG_AWARENESS:
        awarenessProtocol.applyAwarenessUpdate(entry.awareness, decoding.readVarUint8Array(decoder), this)
        break
      case MSG_AUTH: {
        // yrs: varint 0 = denied + reason string, 1 = granted
        const kind = decoding.readVarUint(decoder)
        if (kind === 0) this.onDenied(frame.docId, decoding.readVarString(decoder))
        break
      }
      default:
        break
    }
  }
}
