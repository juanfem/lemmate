// Where things land when you drag them in the file browser. Pure path math, kept out of the
// components because the same rules decide three separate questions: whether a row lights up
// as a drop target, what the confirmation says, and what paths the move writes.
//
// A move inside one vault is a rename (SPEC §4.4 rewrites the `[[links]]`). A move to another
// vault is not — `Workspace.moveNotes` copies the note and its attachments over and deletes the
// original, because a note id belongs to the vault doc that holds it.

import { basename, folderOf } from './tree.ts'

/** What a drag is carrying. One vault: a drag never starts in two of them at once. */
export interface DragPayload {
  vault: string
  /** The notes being moved — for a folder drag, everything inside it. */
  notes: string[]
  /** Set when a folder row started the drag; the notes above are its contents. */
  folder?: string
}

/** Where a note lands when dropped on `dest` (`''` is the vault root). */
export function movedPath(notePath: string, dest: string): string {
  const name = basename(notePath)
  return dest ? `${dest}/${name}` : name
}

/** Where a note inside `folder` lands when `folder` itself is dropped on `dest`. */
export function movedFolderPath(notePath: string, folder: string, dest: string): string {
  const moved = movedPath(folder, dest)
  return `${moved}/${notePath.slice(folder.length + 1)}`
}

/** `a/b` is inside `a`; `a` is not inside itself. */
export function isInside(path: string, folder: string): boolean {
  return folder === '' ? path !== '' : path.startsWith(`${folder}/`)
}

/**
 * Whether a drop is worth offering. Same-vault moves that change nothing are refused rather
 * than accepted-and-ignored, so the row does not light up for a no-op.
 */
export function canDrop(drag: DragPayload, destVault: string, dest: string, paths: (id: string) => string | undefined): boolean {
  if (drag.folder !== undefined) {
    if (destVault !== drag.vault) return true
    // Into itself, into its own subtree, or back where it already is.
    return dest !== drag.folder && !isInside(dest, drag.folder) && folderOf(drag.folder) !== dest
  }
  if (destVault !== drag.vault) return drag.notes.length > 0
  return drag.notes.some((id) => {
    const p = paths(id)
    return p !== undefined && folderOf(p) !== dest
  })
}

/**
 * A free path at `wanted`, sidestepping anything in `taken` as `name 2.md`, `name 3.md`. Used
 * on cross-vault moves, where the target vault knows nothing about the source's names.
 */
export function uniquePath(wanted: string, taken: ReadonlySet<string>): string {
  if (!taken.has(wanted)) return wanted
  const dot = basename(wanted).lastIndexOf('.')
  const cut = dot <= 0 ? wanted.length : wanted.length - (basename(wanted).length - dot)
  const stem = wanted.slice(0, cut)
  const ext = wanted.slice(cut)
  for (let n = 2; ; n++) {
    const candidate = `${stem} ${n}${ext}`
    if (!taken.has(candidate)) return candidate
  }
}

/** Every `path -> newPath` a drop would write, no-ops dropped. */
export function plan(drag: DragPayload, dest: string, paths: (id: string) => string | undefined): { id: string; from: string; to: string }[] {
  const out: { id: string; from: string; to: string }[] = []
  for (const id of drag.notes) {
    const from = paths(id)
    if (from === undefined) continue
    const to = drag.folder !== undefined ? movedFolderPath(from, drag.folder, dest) : movedPath(from, dest)
    if (to !== from) out.push({ id, from, to })
  }
  return out
}

/**
 * The same markdown under a new note id. A note carries its id in its front matter (SPEC
 * §6.3), so a copy into another vault has to be re-stamped or the two would claim one id.
 */
export function restampId(text: string, id: string): string {
  if (!text.startsWith('---\n')) return `---\nid: ${id}\n---\n${text}`
  const close = text.indexOf('\n---', 4)
  if (close === -1) return `---\nid: ${id}\n---\n${text}`
  const front = text.slice(4, close)
  const rest = text.slice(close)
  const lines = front.split('\n')
  const at = lines.findIndex((l) => /^id\s*:/u.test(l))
  if (at === -1) lines.unshift(`id: ${id}`)
  else lines[at] = `id: ${id}`
  return `---\n${lines.join('\n')}${rest}`
}
