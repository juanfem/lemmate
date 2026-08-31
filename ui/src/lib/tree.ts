// Folder tree over a vault's flat `path -> id` note list. Pure and shared: the unified tree
// view, the split folder/notes view and their "reveal the open note" all read the same shape,
// and `test/tree.test.ts` exercises it without a DOM.

import type { NoteEntry } from './vault.svelte.ts'

export interface FolderNode {
  /** Last path segment; `''` for a vault root. */
  name: string
  /** Slash path inside the vault; `''` for the root. */
  path: string
  /** Sub-folders, sorted by name. */
  folders: FolderNode[]
  /** Notes sitting directly in this folder, sorted by path. */
  notes: NoteEntry[]
}

/** The folder a note lives in, `''` when it sits at the vault root. */
export function folderOf(notePath: string): string {
  const i = notePath.lastIndexOf('/')
  return i === -1 ? '' : notePath.slice(0, i)
}

/** `a/b/c` → `['a', 'a/b', 'a/b/c']`; `''` → `[]`. */
export function ancestors(folder: string): string[] {
  if (!folder) return []
  const out: string[] = []
  let cur = ''
  for (const part of folder.split('/')) {
    cur = cur ? `${cur}/${part}` : part
    out.push(cur)
  }
  return out
}

export function buildTree(entries: NoteEntry[]): FolderNode {
  const root: FolderNode = { name: '', path: '', folders: [], notes: [] }
  const byPath = new Map<string, FolderNode>([['', root]])
  for (const n of entries) {
    let cur = root
    for (const part of ancestors(folderOf(n.path))) {
      let next = byPath.get(part)
      if (!next) {
        next = { name: part.slice(part.lastIndexOf('/') + 1), path: part, folders: [], notes: [] }
        byPath.set(part, next)
        cur.folders.push(next)
      }
      cur = next
    }
    cur.notes.push(n)
  }
  for (const f of byPath.values()) {
    f.folders.sort((a, b) => a.name.localeCompare(b.name))
    f.notes.sort((a, b) => a.path.localeCompare(b.path))
  }
  return root
}

/** Notes in this folder and every folder under it. */
export function countNotes(f: FolderNode): number {
  let n = f.notes.length
  for (const sub of f.folders) n += countNotes(sub)
  return n
}

/** Every folder path under `f`, the node itself excluded. */
export function folderPaths(f: FolderNode): string[] {
  const out: string[] = []
  for (const sub of f.folders) {
    out.push(sub.path)
    out.push(...folderPaths(sub))
  }
  return out
}

export function findFolder(root: FolderNode, path: string): FolderNode | undefined {
  if (!path) return root
  let cur: FolderNode | undefined = root
  for (const part of path.split('/')) {
    cur = cur?.folders.find((f) => f.name === part)
    if (!cur) return undefined
  }
  return cur
}

/** The notes to list for a folder: its own, plus every descendant's when `recursive`. */
export function notesIn(root: FolderNode, path: string, recursive: boolean): NoteEntry[] {
  const start = findFolder(root, path)
  if (!start) return []
  if (!recursive) return start.notes
  const out: NoteEntry[] = []
  const walk = (f: FolderNode) => {
    out.push(...f.notes)
    for (const sub of f.folders) walk(sub)
  }
  walk(start)
  return out.sort((a, b) => a.path.localeCompare(b.path))
}

/** One vault as a root of the sidebar tree. */
export interface VaultNode {
  id: string
  label: string
  notes: NoteEntry[]
}

/** Row actions the sidebar views offer on vaults and folders; each is optional per view. */
export interface TreeActions {
  onCreateIn?: (vault: string, folder: string) => void
  onRenameFolder?: (vault: string, folder: string) => void
  onDeleteFolder?: (vault: string, folder: string) => void
  onCreateInVault?: (vault: string) => void
  onRenameVault?: (vault: string) => void
  onImportInto?: (vault: string) => void
  onNewVault?: () => void
}

/** Collapse state is keyed by vault so two vaults with a `Daily/` folder fold apart. */
export function folderKey(vault: string, folder: string): string {
  return `${vault}/${folder}`
}
