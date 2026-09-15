// Dragging a tab within a window (SPEC §9): along its strip, into another pane's strip or page,
// or against a pane's edge to split it. The rules live here, apart from the DOM, so they can be
// tested: the panes pass in, the panes pass out, and nothing else is touched.

/** The part of a pane a tab move cares about. `kind` absent means a note pane. */
export interface TabPane {
  id: number
  tabs: string[]
  active: string | null
  kind?: 'note' | 'history'
}

/** The tab in flight, and the pane it left. */
export interface TabDrag {
  tab: string
  pane: number
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

/** The position `index` really lands on in `strip` (drawn, without the tab): kept in its group. */
export function clampIndex(strip: string[], tab: string, index: number, pinned: string[]): number {
  const pins = strip.filter((t) => pinned.includes(t)).length
  return pinned.includes(tab) ? Math.min(Math.max(index, 0), pins) : Math.min(Math.max(index, pins), strip.length)
}

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
 * A split past `maxPanes` lands in the target pane instead, as dropping on its middle would.
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
  newPane: (tab: string, from: P) => P,
): { panes: P[]; focused: number } | null {
  const src = panes.find((p) => p.id === drag.pane)
  const dst = panes.find((p) => p.id === drop.pane)
  if (!src || !dst || !src.tabs.includes(drag.tab) || src.kind === 'history' || dst.kind === 'history') return null
  const split = 'split' in drop && panes.length < maxPanes ? drop.split : null
  if (split && src === dst && src.tabs.length === 1) return null

  let out = panes.map((p) => (p === src ? without(p, drag.tab) : p))
  let landed: P
  if (split) {
    landed = newPane(drag.tab, src)
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
  const emptied = out.find((p) => p.id === src.id)
  if (emptied && emptied.tabs.length === 0 && out.length > 1) out = out.filter((p) => p !== emptied)
  return { panes: out, focused: out.indexOf(landed) }
}
