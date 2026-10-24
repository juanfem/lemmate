// Whether any drag is over this window. A frame — a rendered note, a deck — is another document:
// a drag over it is heard there and never reaches the pane around it, so a note or tab let go on
// a render would land nowhere. While this is set, frames stop taking the pointer and the pane
// hears the drag like it would over the editor.
//
// Set on `dragstart` (a drag from this window) and `dragenter` (one from another window, or the
// desktop). Cleared when the drag ends here, drops here, or leaves the window: every element it
// enters it later leaves, so the count of the two comes back to zero only when it has gone.
// Not on a mouse move with no button held: WebKit makes up such moves after a layout — the class
// this sets is one — and on macOS it counts the button as up once a drag has begun.

export const dragging = $state({ on: false })

if (typeof window !== 'undefined') {
  let depth = 0
  const on = () => {
    if (!dragging.on) dragging.on = true
  }
  const off = () => {
    depth = 0
    if (dragging.on) dragging.on = false
  }
  // Capture: a pane stops the drop it takes from going any further.
  window.addEventListener('dragstart', on, true)
  window.addEventListener('dragenter', () => (depth++, on()), true)
  // The next element's `dragenter` comes before the last one's `dragleave`, so moving within the
  // window never touches zero.
  window.addEventListener('dragleave', () => --depth <= 0 && off(), true)
  window.addEventListener('dragend', off, true)
  window.addEventListener('drop', off, true)
  // A count gone astray can leave frames deaf until the next drag; a press means none is in flight.
  window.addEventListener('mousedown', off, true)
}
