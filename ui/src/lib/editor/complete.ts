// Autocomplete (SPEC §8): `[[` note names and headings-free targets, `#` tags. Sources are
// supplied by the shell so the editor stays independent of the session/API layer.

import { autocompletion, type Completion, type CompletionContext, type CompletionResult } from '@codemirror/autocomplete'

export interface CompletionSources {
  /** Vault-relative note paths (e.g. `Projects/Plan.md`). */
  notes: () => string[]
  /** Known tags without `#` (e.g. `project/alpha`). */
  tags: () => Promise<string[]> | string[]
}

function stripExt(path: string): string {
  return path.replace(/\.(md|qmd)$/u, '')
}

export function noteCompletions(src: CompletionSources) {
  const wikilink = (cx: CompletionContext): CompletionResult | null => {
    const m = cx.matchBefore(/!?\[\[([^\]\n|#]*)$/u)
    if (!m) return null
    const from = m.from + m.text.indexOf('[[') + 2
    const options: Completion[] = src.notes().map((p) => {
      const stem = stripExt(p)
      const base = stem.includes('/') ? stem.slice(stem.lastIndexOf('/') + 1) : stem
      return { label: base, detail: stem !== base ? stem : undefined, apply: `${stem}]]`, type: 'text' }
    })
    return { from, options, validFor: /^[^\]\n|#]*$/u }
  }
  const tag = async (cx: CompletionContext): Promise<CompletionResult | null> => {
    const m = cx.matchBefore(/(^|\s)#([\p{L}\p{N}_/-]*)$/u)
    if (!m) return null
    const from = m.from + m.text.indexOf('#') + 1
    const tags = await src.tags()
    if (tags.length === 0) return null
    return { from, options: tags.map((t) => ({ label: t, type: 'keyword' })), validFor: /^[\p{L}\p{N}_/-]*$/u }
  }
  return autocompletion({ override: [wikilink, tag], activateOnTyping: true, icons: false })
}
