// Reactive media queries for the shell. The layout asks one question — is the window
// phone-shaped — and the answer changes under the app's feet: a rotated phone, a window
// dragged narrow, a desktop that opens the app in a side panel. Everything else the small
// screen needs (tap targets, hover-revealed rows) is CSS and stays in the components.

/** One box per distinct query, so the whole app watching `NARROW` costs one listener. */
const boxes = new Map<string, { readonly current: boolean }>()

/**
 * `matchMedia` as a rune. The listener lives as long as the page: these queries belong to the
 * shell rather than to any one component, so there is nothing to tear down.
 */
export function media(query: string): { readonly current: boolean } {
  const hit = boxes.get(query)
  if (hit) return hit
  const mql = typeof matchMedia === 'function' ? matchMedia(query) : null
  let current = $state(mql?.matches ?? false)
  mql?.addEventListener('change', (e) => (current = e.matches))
  const box = {
    get current() {
      return current
    },
  }
  boxes.set(query, box)
  return box
}

/**
 * Phone-shaped: the sidebar becomes a drawer over the editor and only the focused pane is on
 * screen. The same number is the breakpoint in every component's stylesheet — change both.
 */
export const NARROW = '(max-width: 720px)'
