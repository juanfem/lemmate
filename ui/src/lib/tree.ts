// Folder tree over a vault's flat `path -> id` note list. Pure and shared: the unified tree
// view, the split folder/notes view and their "reveal the open note" all read the same shape,
// and `test/tree.test.ts` exercises it without a DOM.

import type { NoteEntry } from './vault.svelte.ts'
import type { DragPayload } from './moves.ts'

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

/** The last segment of a path: `a/b/c.md` → `c.md`. */
export function basename(path: string): string {
  const i = path.lastIndexOf('/')
  return i === -1 ? path : path.slice(i + 1)
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

/**
 * What the sidebar can ask the shell to do — the hover buttons on a row, the right-click menu,
 * and the drop at the end of a drag. All optional: an entry with nothing behind it is shown
 * disabled rather than hidden, so the menu keeps the same shape wherever you open it.
 */
export interface TreeActions {
  onCreateIn?: (vault: string, folder: string) => void
  onRenameFolder?: (vault: string, folder: string) => void
  onDeleteFolder?: (vault: string, folder: string) => void
  onCreateInVault?: (vault: string) => void
  onRenameVault?: (vault: string) => void
  onImportInto?: (vault: string) => void
  onNewVault?: () => void
  onRenameNote?: (vault: string, id: string) => void
  onTrashNotes?: (vault: string, ids: string[]) => void
  onOpenInTab?: (id: string) => void
  onOpenInPane?: (id: string) => void
  onShareNote?: (id: string) => void
  onBookmarkNote?: (vault: string, id: string) => void
  /** A completed drag: move what it carries into `folder` of `vault`. */
  onMove?: (drag: DragPayload, toVault: string, folder: string) => void
}

/** Collapse state is keyed by vault so two vaults with a `Daily/` folder fold apart. */
export function folderKey(vault: string, folder: string): string {
  return `${vault}/${folder}`
}

/**
 * What the row components need from `FilesPane` beyond drawing themselves: which notes are
 * selected, and the handlers for clicking, right-clicking and dragging. Bundled rather than
 * passed one prop at a time because three components take the whole set.
 */
export interface BrowserApi {
  /** Note ids currently selected. */
  selected: ReadonlySet<string>
  /** The folder a drag is hovering over, so exactly one row lights up. */
  dropTarget: { vault: string; folder: string } | null
  onNoteClick: (id: string, e: MouseEvent) => void
  onNoteMenu: (id: string, e: MouseEvent) => void
  /** `folder` is `''` for a vault row — a vault root is a folder like any other here. */
  onFolderMenu: (vault: string, folder: string, e: MouseEvent) => void
  onNoteDragStart: (id: string, e: DragEvent) => void
  onFolderDragStart: (vault: string, folder: string, e: DragEvent) => void
  onDragEnd: () => void
  onDragOver: (vault: string, folder: string, e: DragEvent) => void
  onDragLeave: (vault: string, folder: string) => void
  onDrop: (vault: string, folder: string, e: DragEvent) => void
}

/**
 * Note ids in the order the unified tree draws them, folded folders skipped. Shift-click needs
 * a range, and a range only means something against what is actually on screen — so this has
 * to stay in step with `Tree.svelte`: sub-folders first, then the folder's own notes.
 */
export function visibleNotes(vaults: VaultNode[], collapsed: Record<string, boolean>): string[] {
  const out: string[] = []
  const walk = (vault: string, f: FolderNode) => {
    for (const sub of f.folders) {
      if (!collapsed[folderKey(vault, sub.path)]) walk(vault, sub)
    }
    for (const n of f.notes) out.push(n.id)
  }
  for (const v of vaults) {
    if (collapsed[v.id]) continue
    walk(v.id, buildTree(v.notes))
  }
  return out
}

/** The slice of `order` between two ids, in `order`'s own direction. */
export function rangeBetween(order: string[], a: string, b: string): string[] {
  const i = order.indexOf(a)
  const j = order.indexOf(b)
  if (i === -1 || j === -1) return [b]
  return order.slice(Math.min(i, j), Math.max(i, j) + 1)
}
