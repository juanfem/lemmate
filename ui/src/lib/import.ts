// Turning a picked folder into upload batches (SPEC §11.4). The conversion itself is Rust —
// the server and the local relay both run `lemmate_core::import` — so all the browser has to do
// is name each file the way the vault will hold it and keep each request a sensible size.

/** What a folder picker gives us; `File` satisfies it, and tests can use plain objects. */
export interface PickedFile {
  name: string
  size: number
  /** Set by `<input webkitdirectory>`: "<picked folder>/<path inside it>". */
  webkitRelativePath?: string
}

export interface Upload<F extends PickedFile = File> {
  /** Vault-relative path, the name the multipart part carries. */
  path: string
  file: F
}

/** Bytes per request: well under the 64 MiB the server accepts, small enough to feel live. */
export const BATCH_BYTES = 16 * 1024 * 1024
export const BATCH_FILES = 300

/**
 * Vault-relative paths for picked files. `webkitRelativePath` is prefixed with the folder you
 * picked ("MyVault/Daily/x.md"), and that folder *is* the vault root rather than a folder
 * inside it, so the shared prefix comes off. A picker that gave us no relative paths (or files
 * from several roots) leaves the names alone.
 */
export function toUploads<F extends PickedFile>(files: F[]): { uploads: Upload<F>[]; root: string } {
  const paths = files.map((f) => f.webkitRelativePath || f.name)
  const first = paths[0]?.split('/')[0] ?? ''
  const shared = first !== '' && paths.length > 0 && paths.every((p) => p.startsWith(`${first}/`))
  return {
    root: shared ? first : '',
    uploads: files.map((file, i) => ({ path: shared ? paths[i]!.slice(first.length + 1) : paths[i]!, file })),
  }
}

/**
 * Split uploads into requests: whichever of the size or count limit is reached first ends a
 * batch, and a single file over the size limit gets a request to itself rather than being
 * dropped (the server decides whether it is too big to store).
 */
export function batches<F extends PickedFile>(
  files: Upload<F>[],
  limits: { bytes?: number; count?: number } = {},
): Upload<F>[][] {
  const maxBytes = limits.bytes ?? BATCH_BYTES
  const maxFiles = limits.count ?? BATCH_FILES
  const out: Upload<F>[][] = []
  let batch: Upload<F>[] = []
  let bytes = 0
  for (const f of files) {
    if (batch.length > 0 && (bytes + f.file.size > maxBytes || batch.length >= maxFiles)) {
      out.push(batch)
      batch = []
      bytes = 0
    }
    batch.push(f)
    bytes += f.file.size
  }
  if (batch.length) out.push(batch)
  return out
}

export function totalBytes<F extends PickedFile>(files: Upload<F>[]): number {
  return files.reduce((n, f) => n + f.file.size, 0)
}
