// A tab can hold a vault file rather than a note (SPEC §9). Tabs are plain strings — a note's
// id, or `blank:N` — so a file's tab is one too: `file:<vault>:<path>`. A vault id is a ULID and
// has no `:`, so the path is everything after the second one, colons and all.

const PREFIX = 'file:'

export function fileTab(vault: string, path: string): string {
  return `${PREFIX}${vault}:${path}`
}

export function isFileTab(tab: string): boolean {
  return tab.startsWith(PREFIX)
}

export function parseFileTab(tab: string): { vault: string; path: string } | null {
  if (!isFileTab(tab)) return null
  const rest = tab.slice(PREFIX.length)
  const colon = rest.indexOf(':')
  if (colon <= 0 || colon === rest.length - 1) return null
  return { vault: rest.slice(0, colon), path: rest.slice(colon + 1) }
}

/**
 * File tabs with unsaved edits. A file is saved by hand, unlike a note, so closing its tab can
 * lose work; the shell asks first. Each open editor holds its own mark (a file can be open in
 * two panes), keyed by the tab.
 */
const unsaved = new Map<string, Set<symbol>>()

export function setUnsaved(tab: string, holder: symbol, dirty: boolean) {
  const set = unsaved.get(tab) ?? new Set<symbol>()
  if (dirty) set.add(holder)
  else set.delete(holder)
  if (set.size) unsaved.set(tab, set)
  else unsaved.delete(tab)
}

export function hasUnsaved(tab: string): boolean {
  return unsaved.has(tab)
}
