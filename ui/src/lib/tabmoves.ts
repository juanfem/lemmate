// Dragging a tab within a window (SPEC §9): along its strip, into another pane's strip or page,
// or against a pane's edge to split it. The rules live here, apart from the DOM, so they can be
// tested: the panes pass in, the panes pass out, and nothing else is touched.

import { isRenderTab, renderTab } from './rendertabs.ts'

/** The part of a pane a tab move cares about. `kind` absent means a note pane. */
export interface TabPane {
  id: number
  tabs: string[]
  active: string | null
  kind?: 'note' | 'history'
}

/** The tab in flight, and the pane it left — `null` when that pane is in another window. */
export interface TabDrag {
  tab: string
  pane: number | null
}

/**
 * Where it lands. `index` counts positions in the target strip as it is drawn *without* the
 * dragged tab — pinned tabs first — which is what a pointer between two tabs names. A split puts
 * it in a new pane beside the target.
 */
export type TabDrop = { pane: number; index: number } | { pane: number; split: 'left' | 'right' }

/** Pinned tabs sort first (see Pane.svelte), and a drag cannot mix the two groups. */
export function drawn(tabs: string[], pinned: string[]): string[] {
  return [...tabs].sort((a, b) => Number(pinned.includes(b)) - Number(pinned.includes(a)))
}

/** The tab `delta` steps from `active` along the strip as drawn, wrapping at either end. */
export function stepTab(tabs: string[], pinned: string[], active: string | null, delta: number): string | null {
  const strip = drawn(tabs, pinned)
  if (strip.length === 0) return null
  const at = active === null ? -1 : strip.indexOf(active)
  if (at === -1) return strip[0]!
  return strip[(((at + delta) % strip.length) + strip.length) % strip.length]!
}

/** The position `index` really lands on in `strip` (drawn, without the tab): kept in its group. */
export function clampIndex(strip: string[], tab: string, index: number, pinned: string[]): number {
  const pins = strip.filter((t) => pinned.includes(t)).length
  return pinned.includes(tab) ? Math.min(Math.max(index, 0), pins) : Math.min(Math.max(index, pins), strip.length)
}

/** A pane of tabs — not a history pane, which follows one note only. Its tabs are notes,
 *  files and rendered notes, mixed as they were opened or dragged in. */
const isNotes = (p: TabPane) => (p.kind ?? 'note') === 'note'
/** A pane of tabs with a note, or nothing yet, in front — not a rendered one. */
const showsNote = (p: TabPane) => isNotes(p) && !(p.active && isRenderTab(p.active))

function without<P extends TabPane>(p: P, tab: string): P {
  const i = p.tabs.indexOf(tab)
  if (i < 0) return p
  const tabs = p.tabs.filter((t) => t !== tab)
  // The same neighbour closing a tab would pick.
  const active = p.active === tab ? (tabs[Math.min(i, tabs.length - 1)] ?? null) : p.active
  return { ...p, tabs, active }
}

/**
 * Move `drag.tab` to `drop`, or null when the move would change nothing or is not allowed:
 * history panes are neither sources nor targets, and a pane cannot be split off its own only tab.
 * Any other tab — a note, a file, a rendered note — goes into any other pane of tabs.
 * A split past `maxPanes` lands in the target pane instead, as dropping on its middle would.
 *
 * A tab from another window (`drag.pane` null) only arrives: taking it out of the pane it left is
 * that window's business, done by `removeTab` there once it hears the drop was taken.
 *
 * The tab becomes the active one where it lands, and the pane it lands in takes the focus. A pane
 * the move empties goes away, unless it is the last one. The same note already open in the target
 * is not doubled: the move takes that tab's place.
 */
export function moveTab<P extends TabPane>(
  panes: P[],
  drag: TabDrag,
  drop: TabDrop,
  pinned: string[],
  maxPanes: number,
  /** A pane for a split, taking its view mode from `like` — the pane the tab left, if here. */
  newPane: (tab: string, like: P) => P,
): { panes: P[]; focused: number } | null {
  const src = drag.pane === null ? undefined : panes.find((p) => p.id === drag.pane)
  const dst = panes.find((p) => p.id === drop.pane)
  if (!dst || !isNotes(dst)) return null
  if (drag.pane !== null && (!src || !src.tabs.includes(drag.tab) || !isNotes(src))) return null
  const split = 'split' in drop && panes.length < maxPanes ? drop.split : null
  if (split && src === dst && src.tabs.length === 1) return null

  let out = panes.map((p) => (p === src ? without(p, drag.tab) : p))
  let landed: P
  if (split) {
    landed = newPane(drag.tab, src ?? dst)
    const at = out.findIndex((p) => p.id === dst.id)
    out.splice(split === 'left' ? at : at + 1, 0, landed)
  } else {
    const target = out.find((p) => p.id === dst.id)!
    const strip = drawn(
      target.tabs.filter((t) => t !== drag.tab),
      pinned,
    )
    const at = clampIndex(strip, drag.tab, 'index' in drop ? drop.index : strip.length, pinned)
    const tabs = [...strip.slice(0, at), drag.tab, ...strip.slice(at)]
    if (src === dst && tabs.join('\n') === drawn(src.tabs, pinned).join('\n') && src.active === drag.tab) return null
    landed = { ...target, tabs, active: drag.tab }
    out = out.map((p) => (p.id === dst.id ? landed : p))
  }
  const emptied = src && out.find((p) => p.id === src.id)
  if (emptied && emptied.tabs.length === 0 && out.length > 1) out = out.filter((p) => p !== emptied)
  return { panes: out, focused: out.indexOf(landed) }
}

/**
 * Whether a drag that no window took ended outside this one (`width` × `height`): dropped on the
 * desktop, another app, or a window that refused it. Judged by where it ended rather than by a
 * `dragleave` on the way out, because a cancelled drag fires `dragleave` too. An engine that
 * reports no position for the end of a drag (both screen coordinates zero) is taken to have ended
 * inside, so the worst it can do is not open a window.
 */
export function endedOutside(
  e: { screenX: number; screenY: number; clientX: number; clientY: number },
  width: number,
  height: number,
): boolean {
  if (e.screenX === 0 && e.screenY === 0) return false
  return e.clientX < 0 || e.clientY < 0 || e.clientX >= width || e.clientY >= height
}

/**
 * The other half of a move between windows: the tab left `drag.pane` for somewhere else. The pane
 * picks its neighbour as closing the tab would, and goes if that empties it and it is not the last.
 */
export function removeTab<P extends TabPane>(panes: P[], drag: TabDrag): P[] {
  const src = panes.find((p) => p.id === drag.pane)
  if (!src || !src.tabs.includes(drag.tab)) return panes
  const out = panes.map((p) => (p === src ? without(p, drag.tab) : p))
  const emptied = out.find((p) => p.id === src.id)!
  return emptied.tabs.length === 0 && out.length > 1 ? out.filter((p) => p !== emptied) : out
}

/**
 * Where a note opened from outside the panes goes, when the focused pane is a history pane —
 * which only ever shows what it was opened for — or has a rendered note in front, which the note
 * would cover. The focus moves to a pane with a note in front; failing that, a pane of tabs stays
 * where it is (the note becomes a tab of its own beside the render), and a history pane gives way
 * to any pane of tabs, or to a new one when none is left: to the left while there is room, and in
 * the history pane's place when there is not. `fresh` makes the new pane.
 */
export function notePane<P extends TabPane>(panes: P[], focused: number, max: number, fresh: () => P): { panes: P[]; focused: number } {
  const current = panes[focused]
  if (!current || showsNote(current)) return { panes, focused }
  const shows = panes.findIndex(showsNote)
  if (shows >= 0) return { panes, focused: shows }
  if (isNotes(current)) return { panes, focused }
  const i = panes.findIndex(isNotes)
  if (i >= 0) return { panes, focused: i }
  if (panes.length < max) return { panes: [fresh(), ...panes], focused: 0 }
  return { panes: panes.map((p, j) => (j === focused ? fresh() : p)), focused }
}

/**
 * "Open in new pane": a pane of its own, beside the focused one, while there is room. Without
 * room the note goes to the next pane of notes as a tab of its own, so nothing already open is
 * displaced — and a phone has as much room as a desktop: it draws one pane at a time, but the
 * rest are still there, a tap away on the top bar's pane switcher. Treating it as full is what
 * used to put the note over the one you were reading.
 */
export function inNewPane<P extends TabPane>(
  panes: P[],
  focused: number,
  max: number,
  id: string,
  fresh: (tab: string, like: P) => P,
): { panes: P[]; focused: number } {
  const here = panes[focused]
  if (!here) return { panes, focused }
  if (panes.length < max) return { panes: [...panes.slice(0, focused + 1), fresh(id, here), ...panes.slice(focused + 1)], focused: focused + 1 }
  for (let k = 1; k <= panes.length; k++) {
    const j = (focused + k) % panes.length
    const p = panes[j]!
    if (!isNotes(p)) continue
    const tabs = p.tabs.includes(id) ? p.tabs : [...p.tabs, id]
    return { panes: panes.map((q, i) => (i === j ? { ...q, tabs, active: id } : q)), focused: j }
  }
  return { panes, focused }
}

/**
 * Where a note's render opens (SPEC §5.6). As a `tab` — the icon on the strip, the palette —
 * it goes beside the note in the note's own pane: a deck wants the whole width, and a split is
 * one drag away for a page that is better read beside its source. In a `pane` — the `···`
 * menu — it opens beside the note's pane, and later renders gather in a pane already showing
 * one rather than opening another. A render already open is brought forward rather than made
 * twice, except that asking for a pane takes one out of its note's pane into a pane of its own.
 * A history pane has no tabs to add to, so a tab asked for there opens as a pane would.
 */
export function placeRender<P extends TabPane>(
  panes: P[],
  focused: number,
  max: number,
  note: string,
  where: 'tab' | 'pane',
  fresh: (tab: string, like: P) => P,
): { panes: P[]; focused: number } {
  const here = panes[focused]
  if (!here) return { panes, focused }
  const tab = renderTab(note)
  const seen = panes.findIndex((p) => isNotes(p) && p.tabs.includes(tab))
  const moveOut = where === 'pane' && seen === focused && here.tabs.length > 1
  if (seen >= 0 && !moveOut) {
    return { panes: panes.map((p, i) => (i === seen ? { ...p, active: tab } : p)), focused: seen }
  }
  if (where === 'tab' && isNotes(here)) {
    const at = here.active ? here.tabs.indexOf(here.active) + 1 : here.tabs.length
    const tabs = [...here.tabs.slice(0, at), tab, ...here.tabs.slice(at)]
    return { panes: panes.map((p, i) => (i === focused ? { ...p, tabs, active: tab } : p)), focused }
  }
  const out = moveOut ? panes.map((p, i) => (i === focused ? without(p, tab) : p)) : panes
  const home = out.findIndex((p, i) => i !== focused && isNotes(p) && !!p.active && isRenderTab(p.active))
  if (home >= 0) {
    return { panes: out.map((p, i) => (i === home ? { ...p, tabs: [...p.tabs, tab], active: tab } : p)), focused: home }
  }
  return inNewPane(out, focused, max, tab, fresh)
}

/** Breathing room left beside a tab scrolled into view, so it does not sit flush on an edge. */
const SHOW_MARGIN = 8

/**
 * Where a strip should scroll to so that the tab spanning `left`…`right` (in the strip's
 * scrolling coordinates) can be seen — or `scroll` itself when it already can be. The strip's
 * right end, `covered` pixels of it, is the sticky cluster of actions the tabs scroll under: a
 * tab there is as hidden as one off the edge.
 */
export function scrollToShow(left: number, right: number, scroll: number, width: number, covered: number): number {
  const end = scroll + width - covered
  if (left < scroll) return Math.max(0, left - SHOW_MARGIN)
  if (right > end) return Math.max(0, Math.min(left - SHOW_MARGIN, right - (width - covered) + SHOW_MARGIN))
  return scroll
}
