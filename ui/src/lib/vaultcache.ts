// The ids of the vaults this browser knows about, kept so an offline start has something to open.
//
// `y-indexeddb` caches the vault docs and every note opened (SPEC §6.4), but nothing reaches
// them unless a `VaultSession` exists, and sessions are built from the vault list. Offline that
// list cannot be fetched, so without this the app comes up with an empty tree in front of a
// browser full of notes it will not show.
//
// Ids only, deliberately: a note count cached here would be stale the moment anything is
// written, and the only thing the list is used for is deciding which sessions to construct.
//
// localStorage rather than IndexedDB on purpose: this is read once during start-up, before
// anything is awaited, and it must not race the docs it exists to unlock.

const KEY = 'lemmate.vaults'

/** Remember the vaults currently open. Failures are ignored: private mode, quota, no storage. */
export function remember(ids: string[]): void {
  try {
    localStorage.setItem(KEY, JSON.stringify(ids))
  } catch {
    /* the next online start will simply fetch the list again */
  }
}

/** The last known ids, or empty when there are none or the value is unreadable. */
export function recall(): string[] {
  try {
    return parse(localStorage.getItem(KEY))
  } catch {
    return []
  }
}

/** Split out from `recall` so the shape-checking is testable without a browser. */
export function parse(raw: string | null): string[] {
  if (!raw) return []
  let data: unknown
  try {
    data = JSON.parse(raw)
  } catch {
    return []
  }
  if (!Array.isArray(data)) return []
  // Tolerate the older shape, which stored `{ id, notes }` objects.
  return data
    .map((v) => (typeof v === 'string' ? v : (v as { id?: unknown } | null)?.id))
    .filter((id): id is string => typeof id === 'string' && id.length > 0)
}
