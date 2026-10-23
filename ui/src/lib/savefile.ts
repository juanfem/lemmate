// A file made in the app, saved where the reader says. Chromium's save picker asks for a place
// before the file exists, and only while the click that asked still counts — a few seconds, less
// than a deck's PowerPoint takes to make — so the place is asked first and the file written once
// made. Without that picker the file is an ordinary download: a browser set to ask where
// downloads go asks then, and the desktop app always does (`crates/desktop/src/downloads.rs`).

interface Writable {
  write(data: Blob): Promise<void>
  close(): Promise<void>
}
export interface SaveHandle {
  createWritable(): Promise<Writable>
  remove?(): Promise<void>
}
export type SavePicker = (o: { suggestedName: string; types?: { accept: Record<string, string[]> }[] }) => Promise<SaveHandle>

/** The browser's save picker, where it has one. */
export function savePicker(): SavePicker | undefined {
  const w = globalThis as { showSaveFilePicker?: SavePicker }
  return typeof w.showSaveFilePicker === 'function' ? w.showSaveFilePicker.bind(globalThis) : undefined
}

/** `blob` as a download named `name`. */
export function download(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = name
  a.click()
  setTimeout(() => URL.revokeObjectURL(url), 60_000)
}

/**
 * Saves what `make` makes as `name`, a file of MIME `type`. Call it straight from the click,
 * before awaiting anything: the picker opens at once. `make` may come back with nothing to save.
 * False if the reader cancelled the picker (and nothing was made) or there was nothing; a failure
 * to make is thrown, with the empty file the picker left removed.
 */
export async function saveAs(
  name: string,
  type: string,
  make: () => Promise<Blob | null>,
  { picker = savePicker(), fallback = download }: { picker?: SavePicker; fallback?: (blob: Blob, name: string) => void } = {},
): Promise<boolean> {
  if (!picker) {
    const blob = await make()
    if (blob) fallback(blob, name)
    return blob !== null
  }
  const ext = /\.[^.]+$/u.exec(name)?.[0]
  let handle: SaveHandle
  try {
    handle = await picker({ suggestedName: name, types: ext ? [{ accept: { [type]: [ext] } }] : undefined })
  } catch (e) {
    if (e instanceof DOMException && e.name === 'AbortError') return false
    throw e
  }
  let blob: Blob | null
  try {
    blob = await make()
  } catch (e) {
    await handle.remove?.().catch(() => {})
    throw e
  }
  if (!blob) {
    await handle.remove?.().catch(() => {})
    return false
  }
  const out = await handle.createWritable()
  await out.write(blob)
  await out.close()
  return true
}
