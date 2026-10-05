// Keep Tab inside a dialog: the palette and the modal sit over the app, and tabbing out of
// them lands on controls hidden behind the backdrop, with the dialog still waiting for an answer.

const FOCUSABLE = 'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'

/** A Svelte action: Tab and Shift+Tab cycle through `node`'s controls rather than leave it. */
export function trapFocus(node: HTMLElement) {
  const onKey = (e: KeyboardEvent) => {
    if (e.key !== 'Tab') return
    const els = [...node.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((el) => el.getClientRects().length > 0)
    if (els.length === 0) {
      e.preventDefault()
      node.focus()
      return
    }
    const first = els[0]!
    const last = els[els.length - 1]!
    const at = document.activeElement
    if (e.shiftKey && (at === first || at === node)) {
      e.preventDefault()
      last.focus()
    } else if (!e.shiftKey && at === last) {
      e.preventDefault()
      first.focus()
    }
  }
  node.addEventListener('keydown', onKey)
  return {
    destroy() {
      node.removeEventListener('keydown', onKey)
    },
  }
}
