// Mermaid diagrams for live preview. The library is large (well over a megabyte with its
// diagram types), so it is imported the first time a note actually has a ```mermaid block —
// a vault without one never downloads it.

import type { Mermaid } from 'mermaid'

/** A finished render: the diagram, or the parser's complaint. */
export type MermaidResult = { svg: string } | { error: string }

let loading: Promise<Mermaid> | null = null
let initialisedDark: boolean | null = null
let seq = 0

/** Renders by theme and source, so a widget rebuilt by an unrelated edit draws at once. */
const cache = new Map<string, MermaidResult>()
const CACHE_MAX = 200

export function prefersDark(): boolean {
  return typeof matchMedia === 'function' && matchMedia('(prefers-color-scheme: dark)').matches
}

function load(): Promise<Mermaid> {
  loading ??= import('mermaid').then((m) => m.default)
  return loading
}

const key = (source: string, dark: boolean) => `${dark ? 'd' : 'l'}\n${source}`

/** The render already made for this source, if there is one. */
export function cachedMermaid(source: string, dark = prefersDark()): MermaidResult | undefined {
  return cache.get(key(source, dark))
}

export async function renderMermaid(source: string, dark = prefersDark()): Promise<MermaidResult> {
  const k = key(source, dark)
  const hit = cache.get(k)
  if (hit) return hit
  const mermaid = await load()
  if (initialisedDark !== dark) {
    // `strict` (the default, said out loud): notes are shared between accounts, and a diagram
    // must not be a way to run script in someone else's window — labels are sanitised and
    // `click` callbacks are off.
    mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: dark ? 'dark' : 'default' })
    initialisedDark = dark
  }
  let result: MermaidResult
  try {
    const { svg } = await mermaid.render(`lemmate-mermaid-${++seq}`, source)
    result = { svg }
  } catch (e) {
    result = { error: e instanceof Error ? e.message : String(e) }
    // A failed render can leave its scratch element behind in <body>.
    document.getElementById(`dlemmate-mermaid-${seq}`)?.remove()
  }
  if (cache.size >= CACHE_MAX) cache.delete(cache.keys().next().value as string)
  cache.set(k, result)
  return result
}
