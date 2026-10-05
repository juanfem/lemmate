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
  /** How many frames carrying our changes have gone out (or would have, offline). */
  sent: number
  /**
   * The SyncStep1s we have sent and not had answered, each with `sent` as it was then. The
   * server takes a connection's frames in order, so the answer to one comes after it has
   * applied everything we sent before it: when that is everything we have sent at all, the
   * server holds all of our changes.
   */
  probes: number[]
  probeTimer: ReturnType<typeof setTimeout> | null
  /** The server refused this doc on this connection; nothing we send is being kept. */
  denied: boolean
}

/** How long a burst of edits settles before asking the server whether it has them. */
const PROBE_DELAY_MS = 400

export class SyncClient {
  private ws: WebSocket | null = null
  private docs = new Map<string, Entry>()
  private backoff = 1000
  private closed = false
  status: SyncStatus = 'connecting'
  onStatus: (s: SyncStatus) => void = () => {}
  onSynced: (docId: string) => void = () => {}
  /**
   * The server has applied every change of ours to `docId` sent so far — not merely sent us its
   * own state, which is all `onSynced` means. Fires after each confirmed burst of edits, and once
   * after every (re)connect, whether or not there was anything to confirm.
   */
  onAcked: (docId: string) => void = () => {}
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
      sent: 0,
      probes: [],
      probeTimer: null,
      denied: false,
      onUpdate: (update, origin) => {
        if (origin === this) return
        const enc = encoding.createEncoder()
        encoding.writeVarUint(enc, MSG_SYNC)
        syncProtocol.writeUpdate(enc, update)
        this.send(docId, encoding.toUint8Array(enc))
        // Counted even offline: then it travels in the next handshake's SyncStep2 instead.
        entry.sent++
        this.scheduleProbe(docId, entry)
      },
      onAwareness: ({ added, updated, removed }, origin) => {
        // What came from the server goes no further: it already went everywhere it should.
        if (origin === this) return
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
    // Tell the others our cursor is gone while the handler that sends it is still attached:
    // `removeAwarenessStates` emits synchronously, and `send` hands the frame to the socket's
    // buffer at once, so it goes out even if the socket is closed right after.
    awarenessProtocol.removeAwarenessStates(e.awareness, [e.doc.clientID], 'close')
    e.doc.off('update', e.onUpdate)
    e.awareness.off('update', e.onAwareness)
    if (e.probeTimer) clearTimeout(e.probeTimer)
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
    // Answers to SyncStep1s sent on a connection that is gone will never come.
    entry.probes = []
    entry.denied = false
    this.probe(docId, entry)
    // Push our awareness state right away so cursors show up on the other side.
    const local = entry.awareness.getLocalState()
    if (local) entry.onAwareness({ added: [], updated: [entry.doc.clientID], removed: [] }, 'handshake')
  }

  /** Send a SyncStep1: the start of a handshake, or a question whether the server has caught up. */
  private probe(docId: string, entry: Entry) {
    if (this.ws?.readyState !== WebSocket.OPEN) return
    const enc = encoding.createEncoder()
    encoding.writeVarUint(enc, MSG_SYNC)
    syncProtocol.writeSyncStep1(enc, entry.doc)
    entry.probes.push(entry.sent)
    this.send(docId, encoding.toUint8Array(enc))
  }

  private scheduleProbe(docId: string, entry: Entry) {
    if (entry.probeTimer) return
    entry.probeTimer = setTimeout(() => {
      entry.probeTimer = null
      // Before the handshake is answered there is nothing to ask: its answer will do.
      if (entry.synced && this.docs.get(docId) === entry) this.probe(docId, entry)
    }, PROBE_DELAY_MS)
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
        // The server answers each SyncStep1 of ours with its SyncStep2 (what we lack) and then
        // a SyncStep1 of its own. Only the handshake's needs a reply: the SyncStep2 that carries
        // what we hold and the server does not — anything made offline. Later ones answer a
        // probe, and what we have made since went out as updates already.
        const peek = decoding.clone(decoder)
        if (decoding.readVarUint(peek) === syncProtocol.messageYjsSyncStep1 && entry.synced) {
          this.answered(frame.docId, entry)
          break
        }
        const enc = encoding.createEncoder()
        encoding.writeVarUint(enc, MSG_SYNC)
        const kind = syncProtocol.readSyncMessage(decoder, enc, entry.doc, this)
        if (encoding.length(enc) > 1) this.send(frame.docId, encoding.toUint8Array(enc))
        if (kind === syncProtocol.messageYjsSyncStep1) {
          // We hold the server's state now (its SyncStep2 came first); it is about to hold ours.
          entry.sent++
          entry.synced = true
          this.onSynced(frame.docId)
          this.answered(frame.docId, entry)
        }
        break
      }
      case MSG_AWARENESS:
        awarenessProtocol.applyAwarenessUpdate(entry.awareness, decoding.readVarUint8Array(decoder), this)
        break
      case MSG_AUTH: {
        // yrs: varint 0 = denied + reason string, 1 = granted
        const kind = decoding.readVarUint(decoder)
        if (kind === 0) {
          entry.denied = true
          this.onDenied(frame.docId, decoding.readVarString(decoder))
        }
        break
      }
      default:
        break
    }
  }

  /** A SyncStep1 of ours has been answered: acknowledge, or ask again for what came since. */
  private answered(docId: string, entry: Entry) {
    const upTo = entry.probes.shift()
    if (upTo === undefined) return
    if (upTo === entry.sent) {
      if (!entry.denied) this.onAcked(docId)
    } else if (entry.probes.length === 0) {
      // Sent more since that question was asked (the handshake's own SyncStep2, at least).
      if (entry.probeTimer) clearTimeout(entry.probeTimer)
      entry.probeTimer = null
      this.probe(docId, entry)
    }
  }
}
