// Where the offline search index lives: one IndexedDB record per cached note.
//
// This is also the record of *what* is cached and at which version, which the version map in
// localStorage used to be. One store rather than two: they would otherwise be written at
// different moments and drift, and a note counted as cached but missing from the index is a
// note that quietly stops being findable.
//
// Everything here fails soft. Private windows, a browser with storage disabled, a quota that
// has run out — none of them should stop the app working online, so a failure means an empty
// index and a search that goes to the server.

const DB_NAME = 'lemmate-search'
const STORE = 'notes'
const VAULT_INDEX = 'vault'

export interface StoredNote {
  /** `<vault>:<note>` — unique across vaults, which note ids already are, but explicit. */
  key: string
  vault: string
  id: string
  /** The server's `updated_at` when this text was fetched; how staleness is spotted. */
  version: string
  title: string | null
  /** Markup-free text, from the same indexer the server runs (SPEC §5). */
  text: string
}

export const noteKey = (vault: string, id: string) => `${vault}:${id}`

function open(): Promise<IDBDatabase | null> {
  return new Promise((resolve) => {
    let request: IDBOpenDBRequest
    try {
      request = indexedDB.open(DB_NAME, 1)
    } catch {
      return resolve(null)
    }
    request.onupgradeneeded = () => {
      const db = request.result
      if (!db.objectStoreNames.contains(STORE)) {
        db.createObjectStore(STORE, { keyPath: 'key' }).createIndex(VAULT_INDEX, 'vault')
      }
    }
    request.onsuccess = () => resolve(request.result)
    request.onerror = () => resolve(null)
    request.onblocked = () => resolve(null)
  })
}

function done(tx: IDBTransaction): Promise<boolean> {
  return new Promise((resolve) => {
    tx.oncomplete = () => resolve(true)
    tx.onerror = () => resolve(false)
    tx.onabort = () => resolve(false)
  })
}

/** Everything cached for one vault, by note id. Empty when there is no usable storage. */
export async function loadVault(vault: string): Promise<Map<string, StoredNote>> {
  const out = new Map<string, StoredNote>()
  const db = await open()
  if (!db) return out
  try {
    const rows = await new Promise<StoredNote[]>((resolve) => {
      const request = db.transaction(STORE, 'readonly').objectStore(STORE).index(VAULT_INDEX).getAll(vault)
      request.onsuccess = () => resolve(request.result as StoredNote[])
      request.onerror = () => resolve([])
    })
    for (const row of rows) out.set(row.id, row)
  } catch {
    /* the store shape changed under us, or the transaction could not start */
  } finally {
    db.close()
  }
  return out
}

export async function putNotes(notes: StoredNote[]): Promise<void> {
  if (notes.length === 0) return
  const db = await open()
  if (!db) return
  try {
    const tx = db.transaction(STORE, 'readwrite')
    const store = tx.objectStore(STORE)
    for (const note of notes) store.put(note)
    await done(tx)
  } catch {
    /* quota, most likely: the index simply stays as complete as it managed to get */
  } finally {
    db.close()
  }
}

/** Forget notes that no longer exist, so a deleted note stops turning up in results. */
export async function removeNotes(keys: string[]): Promise<void> {
  if (keys.length === 0) return
  const db = await open()
  if (!db) return
  try {
    const tx = db.transaction(STORE, 'readwrite')
    const store = tx.objectStore(STORE)
    for (const key of keys) store.delete(key)
    await done(tx)
  } catch {
    /* nothing to do: a stale entry is a wrong result, not a broken app */
  } finally {
    db.close()
  }
}
