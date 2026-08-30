// Sync frame codec (SPEC §7): u16 BE doc-id length | doc id | Yjs protocol message.

export interface Frame {
  docId: string
  payload: Uint8Array
}

const enc = new TextEncoder()
const dec = new TextDecoder()

export function encodeFrame(docId: string, payload: Uint8Array): Uint8Array {
  const id = enc.encode(docId)
  const out = new Uint8Array(2 + id.length + payload.length)
  out[0] = id.length >> 8
  out[1] = id.length & 0xff
  out.set(id, 2)
  out.set(payload, 2 + id.length)
  return out
}

export function decodeFrame(bytes: Uint8Array): Frame {
  if (bytes.length < 2) throw new Error('frame too short')
  const n = (bytes[0]! << 8) | bytes[1]!
  if (bytes.length < 2 + n) throw new Error('doc id truncated')
  return { docId: dec.decode(bytes.subarray(2, 2 + n)), payload: bytes.subarray(2 + n) }
}
