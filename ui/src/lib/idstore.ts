// Small sets of note ids that have to outlive a reload, kept in localStorage.
//
// Two of them, both per vault (see vault.svelte.ts):
//
// * **pending** — notes edited here whose changes the server has not acknowledged. Losing this
//   set loses the edits: a note doc is only pushed while it is subscribed on the socket, and
//   nothing re-subscribes a note you closed before going offline.
// * **cached** — notes whose content has been pulled for offline reading, so a later start does
//   not walk the whole vault again.
//
// localStorage rather than IndexedDB for the same reason as lib/vaultcache.ts: these are read
// once during construction, before anything is awaited, and must not race the docs they describe.

/** Split out from `loadIds` so the shape-checking is testable without a browser. */
export function parseIds(raw: string | null): string[] {
  if (!raw) return []
  let data: unknown
  try {
    data = JSON.parse(raw)
  } catch {
    return []
  }
  if (!Array.isArray(data)) return []
  return data.filter((id): id is string => typeof id === 'string' && id.length > 0)
}

export function loadIds(key: string): Set<string> {
  try {
    return new Set(parseIds(localStorage.getItem(key)))
  } catch {
    return new Set()
  }
}

export function saveIds(key: string, ids: Iterable<string>): void {
  try {
    localStorage.setItem(key, JSON.stringify([...ids]))
  } catch {
    /* private mode or quota: the set is a cache, and losing it costs a re-sync, not data */
  }
}
