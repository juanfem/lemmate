// Pointer-drag for the two splitters in the shell (sidebar width, folders/notes height).
// Pointer capture keeps the drag alive over the editor and outside the window, so there is no
// document-level listener to leak.

export interface DragOptions {
  /** Which coordinate the handle follows. */
  axis: 'x' | 'y'
  /** Size at the start of the drag, in px. */
  from: number
  min: number
  max: number
  onMove: (size: number) => void
  onEnd?: (size: number) => void
}

export function clamp(v: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, v))
}

/** Call from `onpointerdown` on the handle. */
export function dragResize(e: PointerEvent, opts: DragOptions): void {
  const handle = e.currentTarget as HTMLElement | null
  if (!handle) return
  e.preventDefault()
  try {
    handle.setPointerCapture(e.pointerId)
  } catch {
    /* synthetic pointer (tests): the drag still works, it just stops at the handle */
  }
  const origin = opts.axis === 'x' ? e.clientX : e.clientY
  let size = opts.from
  const move = (ev: PointerEvent) => {
    const at = opts.axis === 'x' ? ev.clientX : ev.clientY
    size = clamp(opts.from + at - origin, opts.min, opts.max)
    opts.onMove(size)
  }
  const done = () => {
    handle.removeEventListener('pointermove', move)
    handle.removeEventListener('pointerup', done)
    handle.removeEventListener('pointercancel', done)
    try {
      handle.releasePointerCapture(e.pointerId)
    } catch {
      /* already gone */
    }
    opts.onEnd?.(size)
  }
  handle.addEventListener('pointermove', move)
  handle.addEventListener('pointerup', done)
  handle.addEventListener('pointercancel', done)
}
