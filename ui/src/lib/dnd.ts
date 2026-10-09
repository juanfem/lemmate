// The drag in flight. `dragover` has to decide whether a row is a legal drop target, and the
// HTML drag-and-drop API will not let it read `dataTransfer` until the drop — so the payload
// is kept here as well. Module state is right for it: there is exactly one drag at a time, and
// it never outlives the gesture.

import type { DragPayload } from './moves.ts'

const MIME = 'application/x-lemmate-notes'

let current: DragPayload | null = null

/** `text` is what the drag gives anything outside the app: a note's wikilink, a folder's path. */
export function beginDrag(e: DragEvent, payload: DragPayload, text: string) {
  current = payload
  if (!e.dataTransfer) return
  e.dataTransfer.effectAllowed = 'move'
  // Also on the event, so a drop that somehow outlives this module still knows what it holds.
  e.dataTransfer.setData(MIME, JSON.stringify(payload))
  e.dataTransfer.setData('text/plain', text)
}

export function readDrag(e?: DragEvent): DragPayload | null {
  if (current) return current
  const raw = e?.dataTransfer?.getData(MIME)
  if (!raw) return null
  try {
    return JSON.parse(raw) as DragPayload
  } catch {
    return null
  }
}

/**
 * Notes a pane could open: a drag from this window's sidebar, or one from another window's, which
 * is only recognised by its type until the drop (and may yet turn out to be a folder).
 */
export function carriesNotes(e: DragEvent): boolean {
  if (current) return current.folder === undefined && current.notes.length > 0
  return e.dataTransfer?.types.includes(MIME) ?? false
}

/** The notes a drop on a pane opens — none for a folder, whose drop is a move. */
export function droppedNotes(e: DragEvent): string[] {
  const drag = readDrag(e)
  return drag && drag.folder === undefined ? drag.notes : []
}

export function endDrag() {
  current = null
}

/** Files dragged in from the desktop are the editor's business, not the tree's. */
export function isFileDrag(e: DragEvent): boolean {
  return !current && (e.dataTransfer?.types.includes('Files') ?? false)
}
