// BLAKE3 for attachment content addressing (SPEC §4.4) — must match the Rust `blake3` crate.
import { blake3 } from '@noble/hashes/blake3.js'

export async function blake3Hex(bytes: Uint8Array): Promise<string> {
  return Array.from(blake3(bytes), (b) => b.toString(16).padStart(2, '0')).join('')
}
