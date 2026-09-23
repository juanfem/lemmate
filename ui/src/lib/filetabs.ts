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
