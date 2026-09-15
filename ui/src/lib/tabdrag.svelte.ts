// The tab drag in flight, shared by every pane in the window: `dragover` fires on the pane under
// the pointer, but the pane it left has to fade its tab and the one it passed over has to take
// its marker down. One drag at a time, never outliving the gesture — the same shape as `dnd.ts`
// for the tree, and kept apart from it so a tab can never be dropped on a folder.
//
// A drag that started in another window has no `current` here: this module is per window. It is
// recognised by its type instead, and the tab it carries can only be read at the drop — the
// drag-and-drop API hides the data until then.

import type { TabDrag, TabDrop } from './tabmoves.ts'

const MIME = 'application/x-lemmate-tab'

export const tabDrag = $state<{ current: TabDrag | null; over: TabDrop | null }>({ current: null, over: null })

export function beginTabDrag(e: DragEvent, drag: TabDrag) {
  tabDrag.current = drag
  tabDrag.over = null
  if (!e.dataTransfer) return
  e.dataTransfer.effectAllowed = 'move'
  // Some engines will not start a drag that carries no data. Not `text/plain`: the editor would
  // take that as text to paste wherever the tab was let go.
  e.dataTransfer.setData(MIME, JSON.stringify(drag))
}

/** Where the drag would land now, set only when that changes so the markers do not churn. */
export function hoverTab(over: TabDrop | null) {
  const was = tabDrag.over
  const same =
    was === over ||
    (was !== null &&
      over !== null &&
      was.pane === over.pane &&
      ('index' in was ? 'index' in over && was.index === over.index : 'split' in over && was.split === over.split))
  if (!same) tabDrag.over = over
}

/** A tab drag, from this window or another one on the same origin. */
export function carriesTab(e: DragEvent): boolean {
  return tabDrag.current !== null || (e.dataTransfer?.types.includes(MIME) ?? false)
}

/** The tab a drop from another window carries; only readable during `drop`. */
export function droppedTab(e: DragEvent): string | null {
  try {
    const drag = JSON.parse(e.dataTransfer?.getData(MIME) || 'null') as { tab?: unknown } | null
    return typeof drag?.tab === 'string' && drag.tab ? drag.tab : null
  } catch {
    return null
  }
}

export function endTabDrag() {
  tabDrag.current = null
  tabDrag.over = null
}
