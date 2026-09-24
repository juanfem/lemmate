// A rendered note is a tab like any other (SPEC §5.6, §9): it sits among notes, moves between
// panes and windows, pins and closes the same way. Tabs are plain strings, so it is one too —
// `render:<note>` — and the pane shows what Quarto makes of the note instead of its text.

const PREFIX = 'render:'

export function renderTab(note: string): string {
  return `${PREFIX}${note}`
}

export function isRenderTab(tab: string): boolean {
  return tab.startsWith(PREFIX)
}

/** The note a tab is about: a rendered note's tab names it, and any other tab is its own. */
export function tabNote(tab: string): string {
  return isRenderTab(tab) ? tab.slice(PREFIX.length) : tab
}
