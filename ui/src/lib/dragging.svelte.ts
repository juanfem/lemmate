// Whether any drag is over this window. A frame — a rendered note, a deck — is another document:
// a drag over it is heard there and never reaches the pane around it, so a note or tab let go on
// a render would land nowhere. While this is set, frames stop taking the pointer and the pane
// hears the drag like it would over the editor.
//
// Set on `dragstart` (a drag from this window) and `dragenter` (one from another window, or the
// desktop). Cleared when the drag ends here, drops here, or — for a drag that only passed through —
// on the next mouse move with no button held, which no drag in flight makes.

export const dragging = $state({ on: false })

if (typeof window !== 'undefined') {
  const on = () => {
    if (!dragging.on) dragging.on = true
  }
  const off = () => {
    if (dragging.on) dragging.on = false
  }
  // Capture: a pane stops the drop it takes from going any further.
  window.addEventListener('dragstart', on, true)
  window.addEventListener('dragenter', on, true)
  window.addEventListener('dragend', off, true)
  window.addEventListener('drop', off, true)
  window.addEventListener('mousemove', (e) => e.buttons === 0 && off(), true)
}
