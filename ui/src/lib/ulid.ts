// ULID (Crockford base32, 48-bit time + 80-bit randomness) — matches the Rust `ulid` crate.
const ALPHABET = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'

export function ulid(now: number = Date.now()): string {
  let time = ''
  let t = now
  for (let i = 0; i < 10; i++) {
    time = ALPHABET[t % 32] + time
    t = Math.floor(t / 32)
  }
  const bytes = new Uint8Array(10)
  crypto.getRandomValues(bytes)
  // 80 bits → 16 base32 chars
  let bits = 0n
  for (const b of bytes) bits = (bits << 8n) | BigInt(b)
  let rand = ''
  for (let i = 0; i < 16; i++) {
    rand = ALPHABET[Number(bits & 31n)] + rand
    bits >>= 5n
  }
  return time + rand
}

export const ULID_RE = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/
