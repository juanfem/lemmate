// Touch has no right button. Holding a row fires a `contextmenu` event at the finger, so every
// `oncontextmenu` handler in the file browser — rename, share, trash, copy path — is reachable
// on a phone without a second code path. Drag-and-drop is *not* rescued this way: HTML5 DnD
// never fires on touch at all, so the menu's "Rename / move…" is the move gesture there.

/** How long the finger must rest before the menu opens. */
export const HOLD_MS = 500
/** How far it may drift first: past this the gesture is a scroll, not a press. */
export const SLOP_PX = 10

export interface Point {
  x: number
  y: number
}

export interface LongPressOptions {
  /** Called once the finger has rested for `hold` ms without drifting past `slop`. */
  onHold: (at: Point) => void
  hold?: number
  slop?: number
  /** Injected so the gesture can be exercised without a real clock. */
  setTimer?: (fn: () => void, ms: number) => unknown
  clearTimer?: (handle: unknown) => void
}

export interface LongPressGesture {
  /** A pointer went down. `pointerType` comes straight off the event; mice are ignored. */
  down: (at: Point, pointerType?: string) => void
  move: (at: Point) => void
  /** The pointer lifted, was cancelled, or the platform opened its own menu first. */
  end: () => void
  /**
   * Whether the click now arriving is the tail of a press that already opened a menu. True at
   * most once per press, and reading it disarms.
   */
  swallowClick: () => boolean
}

/**
 * The gesture with no DOM in it — this is the part worth testing; `longpress` below is wiring.
 * A mouse keeps its own right button, so a mouse press never arms this.
 */
export function longPress(opts: LongPressOptions): LongPressGesture {
  const hold = opts.hold ?? HOLD_MS
  const slop = opts.slop ?? SLOP_PX
  const setTimer = opts.setTimer ?? ((fn, ms) => setTimeout(fn, ms))
  const clearTimer = opts.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>))

  let origin: Point | null = null
  let handle: unknown = null
  let armed = false

  function stop() {
    if (handle !== null) clearTimer(handle)
    handle = null
    origin = null
  }

  function fire() {
    const at = origin
    handle = null
    origin = null
    if (!at) return
    armed = true
    opts.onHold(at)
  }

  return {
    down(at, pointerType) {
      // A fresh gesture: never let a swallow left over from an abandoned press eat this click.
      armed = false
      stop()
      if (pointerType === 'mouse') return
      origin = { x: at.x, y: at.y }
      handle = setTimer(fire, hold)
    },
    move(at) {
      if (!origin) return
      if (Math.abs(at.x - origin.x) > slop || Math.abs(at.y - origin.y) > slop) stop()
    },
    end: stop,
    swallowClick() {
      const was = armed
      armed = false
      return was
    },
  }
}

/**
 * Svelte action. Put it on any row that already has an `oncontextmenu`:
 * `<button use:longpress oncontextmenu={…}>`.
 */
export function longpress(node: HTMLElement): { destroy: () => void } {
  const gesture = longPress({
    onHold: (at) => {
      navigator.vibrate?.(8)
      node.dispatchEvent(
        new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX: at.x, clientY: at.y }),
      )
    },
  })
  const down = (e: PointerEvent) => gesture.down({ x: e.clientX, y: e.clientY }, e.pointerType)
  const move = (e: PointerEvent) => gesture.move({ x: e.clientX, y: e.clientY })
  const end = () => gesture.end()
  // The finger that opened the menu still has to lift, and that lift is a click on the row
  // underneath — which would open the note and dismiss the menu in the same breath.
  const click = (e: MouseEvent) => {
    if (!gesture.swallowClick()) return
    e.preventDefault()
    e.stopImmediatePropagation()
  }

  node.addEventListener('pointerdown', down)
  node.addEventListener('pointermove', move)
  node.addEventListener('pointerup', end)
  node.addEventListener('pointercancel', end)
  // The platform's own long-press menu beat us to it; do not open a second one behind it.
  node.addEventListener('contextmenu', end)
  node.addEventListener('click', click, true)

  return {
    destroy() {
      gesture.end()
      node.removeEventListener('pointerdown', down)
      node.removeEventListener('pointermove', move)
      node.removeEventListener('pointerup', end)
      node.removeEventListener('pointercancel', end)
      node.removeEventListener('contextmenu', end)
      node.removeEventListener('click', click, true)
    },
  }
}
