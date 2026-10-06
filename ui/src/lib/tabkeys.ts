// The keys that move along a pane's tabs and close one. A browser keeps Ctrl+Tab, Ctrl+PgUp/PgDn
// and Ctrl+W for its own tabs, so each has a stand-in a page does receive: Ctrl+Shift+[ / ]
// (JupyterLab's — real Ctrl on a Mac too, where Cmd+Shift+[ / ] is Chrome's) and Alt+W.
// Ctrl+Tab is still bound: wherever the browser is not in the way (the desktop shell) it works.

export type TabKey = { step: 1 | -1 } | 'close'

export function tabKey(e: Pick<KeyboardEvent, 'key' | 'code' | 'ctrlKey' | 'shiftKey' | 'altKey' | 'metaKey'>): TabKey | null {
  if (e.metaKey) return null
  if (e.ctrlKey && !e.altKey && e.key === 'Tab') return { step: e.shiftKey ? -1 : 1 }
  // By code, not key: with Shift held the key is `{` / `}` on most layouts.
  if (e.ctrlKey && e.shiftKey && !e.altKey && (e.code === 'BracketLeft' || e.code === 'BracketRight'))
    return { step: e.code === 'BracketLeft' ? -1 : 1 }
  // By code too: Option+W on a Mac reports `∑`.
  if (e.altKey && !e.ctrlKey && !e.shiftKey && e.code === 'KeyW') return 'close'
  return null
}
