// The attachments view's tree (SPEC §9): a vault's files that are not notes, by folder. Pure —
// entries in, nodes out — so the rules are tested apart from the DOM.

import type { FileEntry } from './api.ts'

export type FileKind = 'image' | 'style' | 'other'

const IMAGE = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'avif', 'bmp', 'ico'])
/** Styles and configuration: what a note's front matter or a render reads. */
const STYLE = new Set(['css', 'scss', 'sass', 'yml', 'yaml', 'lua', 'tex', 'csl', 'bib', 'json', 'toml'])
/** Opened in the editor rather than shown: anything that is text to a person. */
const TEXT = new Set([
  ...STYLE,
  'txt', 'csv', 'tsv', 'xml', 'html', 'htm', 'js', 'mjs', 'ts', 'py', 'r', 'jl', 'sh', 'ini', 'cfg', 'conf', 'typ', 'svg',
])

export function extension(path: string): string {
  const name = path.slice(path.lastIndexOf('/') + 1)
  const dot = name.lastIndexOf('.')
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : ''
}

export function fileKind(path: string): FileKind {
  const ext = extension(path)
  return IMAGE.has(ext) ? 'image' : STYLE.has(ext) ? 'style' : 'other'
}

/** Whether the file opens as text, to be read and edited, rather than as a preview. */
export function isText(path: string): boolean {
  return TEXT.has(extension(path))
}

export function baseName(path: string): string {
  return path.slice(path.lastIndexOf('/') + 1)
}

export function folderOf(path: string): string {
  const i = path.lastIndexOf('/')
  return i === -1 ? '' : path.slice(0, i)
}

export type FileNode =
  | {
      kind: 'folder'
      /** What the row says: one folder's name, or a chain of them (`Archive / 2024`). */
      label: string
      /** The deepest folder of the chain, vault-relative. */
      path: string
      /** Files anywhere below. */
      count: number
      children: FileNode[]
    }
  | { kind: 'file'; name: string; path: string; entry: FileEntry }

/**
 * The files as a tree: folders first, then files, each by name. Only folders that hold files
 * appear — this is not the notes' tree — and a folder that holds nothing but one other folder
 * folds into it (`Archive / 2024 / Old decks`), so a deep, sparse path costs one row.
 */
export function buildFileTree(files: FileEntry[], keep: (f: FileEntry) => boolean = () => true): FileNode[] {
  type Dir = { dirs: Map<string, Dir>; files: FileEntry[] }
  const root: Dir = { dirs: new Map(), files: [] }
  for (const f of files) {
    if (!keep(f)) continue
    const parts = f.path.split('/')
    let dir = root
    for (const seg of parts.slice(0, -1)) {
      let next = dir.dirs.get(seg)
      if (!next) dir.dirs.set(seg, (next = { dirs: new Map(), files: [] }))
      dir = next
    }
    dir.files.push(f)
  }
  const byName = (a: string, b: string) => a.localeCompare(b, undefined, { sensitivity: 'base', numeric: true })
  const nodes = (dir: Dir, prefix: string): FileNode[] => {
    const folders: FileNode[] = [...dir.dirs.entries()]
      .sort(([a], [b]) => byName(a, b))
      .map(([name, sub]) => {
        let label = name
        let path = prefix + name
        let inner = sub
        while (inner.files.length === 0 && inner.dirs.size === 1) {
          const [only, next] = [...inner.dirs.entries()][0]!
          label += ` / ${only}`
          path += `/${only}`
          inner = next
        }
        const children = nodes(inner, `${path}/`)
        return { kind: 'folder' as const, label, path, count: countFiles(children), children }
      })
    const leaves: FileNode[] = [...dir.files]
      .sort((a, b) => byName(baseName(a.path), baseName(b.path)))
      .map((entry) => ({ kind: 'file' as const, name: baseName(entry.path), path: entry.path, entry }))
    return [...folders, ...leaves]
  }
  return nodes(root, '')
}

function countFiles(nodes: FileNode[]): number {
  return nodes.reduce((n, c) => n + (c.kind === 'file' ? 1 : c.count), 0)
}

/** The folders (their `path`s) to open so that every one of `paths` shows. */
export function foldersToReveal(paths: string[]): string[] {
  const out = new Set<string>()
  for (const p of paths) {
    const parts = p.split('/').slice(0, -1)
    for (let i = 1; i <= parts.length; i++) out.add(parts.slice(0, i).join('/'))
  }
  return [...out]
}

/**
 * Where an upload named `name` would land in `folder` without replacing anything: the name
 * itself when it is free, else `name-2`, `name-3`… before the extension.
 */
export function freeName(taken: (path: string) => boolean, folder: string, name: string): string {
  const at = (n: string) => (folder ? `${folder}/${n}` : n)
  if (!taken(at(name))) return at(name)
  const dot = name.lastIndexOf('.')
  const [stem, ext] = dot > 0 ? [name.slice(0, dot), name.slice(dot)] : [name, '']
  for (let i = 2; ; i++) {
    const candidate = at(`${stem}-${i}${ext}`)
    if (!taken(candidate)) return candidate
  }
}

/** A size a person reads: `312 B`, `1.2 KB`, `4.6 MB`. */
export function humanSize(bytes: number | null): string {
  if (bytes === null) return ''
  if (bytes < 1024) return `${bytes} B`
  const units = ['KB', 'MB', 'GB']
  let n = bytes / 1024
  let u = 0
  while (n >= 1024 && u < units.length - 1) {
    n /= 1024
    u++
  }
  return `${n < 10 ? n.toFixed(1) : Math.round(n)} ${units[u]}`
}
