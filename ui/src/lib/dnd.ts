// The drag in flight. `dragover` has to decide whether a row is a legal drop target, and the
// HTML drag-and-drop API will not let it read `dataTransfer` until the drop — so the payload
// is kept here as well. Module state is right for it: there is exactly one drag at a time, and
// it never outlives the gesture.

import type { DragPayload } from './moves.ts'

const MIME = 'application/x-lemmate-notes'

let current: DragPayload | null = null

export function beginDrag(e: DragEvent, payload: DragPayload) {
  current = payload
  if (!e.dataTransfer) return
  e.dataTransfer.effectAllowed = 'move'
  // Also on the event, so a drop that somehow outlives this module still knows what it holds.
  e.dataTransfer.setData(MIME, JSON.stringify(payload))
  e.dataTransfer.setData('text/plain', payload.folder ?? payload.notes.join('\n'))
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

export function endDrag() {
  current = null
}

/** Files dragged in from the desktop are the editor's business, not the tree's. */
export function isFileDrag(e: DragEvent): boolean {
  return !current && (e.dataTransfer?.types.includes('Files') ?? false)
}
