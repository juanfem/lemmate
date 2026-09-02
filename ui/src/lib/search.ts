// Searching the notes this browser has cached, for when the server cannot be reached.
//
// The server's FTS is the real thing (SQLite FTS5, bm25) and is used whenever it can be
// reached. This is the offline stand-in over the same two fields SQLite indexes — the title and
// the markup-free `plain_text` from `markdown/index.ts` — with snippets in the same `[matched]`
// … shape, so a hit reads the same wherever it came from.
//
// It does not give the same *results*, and pretending otherwise would be the wrong kind of
// tidy. Two differences, both deliberate:
//
// * **Substrings, not tokens.** FTS5 matches whole tokens, so searching `invoice` on the server
//   does not find a note about `invoices`. Here it does. For a personal vault that is the more
//   useful answer more often, and it is the only thing a few hundred notes in a browser can
//   afford — there is no token index, just the text.
// * **Counting, not bm25.** Ranking is occurrence counts with a heavy bias towards titles.
//
// So offline search is broader and more crudely ordered. The pane says it is offline while this
// is in use, because a result list that quietly changes its rules is worse than one that admits
// to them.

/** One note as this browser holds it. */
export interface IndexedNote {
  id: string
  vault: string
  title: string | null
  /** Markup-free text, exactly what the server puts in the FTS `body` column. */
  text: string
}

export interface LocalHit {
  note_id: string
  title: string | null
  snippet: string
}

/** Words of context either side of the match, matching the server's `snippet(…, 12)`. */
const CONTEXT_WORDS = 6
/** A title hit is worth this many body hits — the note *called* "Invoices" beats one mentioning it. */
const TITLE_WEIGHT = 8

export function terms(query: string): string[] {
  return query.toLowerCase().split(/\s+/u).filter((t) => t.length > 0)
}

/**
 * Notes matching every term, best first. `limit` mirrors the server's default page.
 */
export function searchNotes(notes: IndexedNote[], query: string, limit = 30): LocalHit[] {
  const wanted = terms(query)
  if (wanted.length === 0) return []
  const scored: { note: IndexedNote; score: number }[] = []
  for (const note of notes) {
    const title = (note.title ?? '').toLowerCase()
    const body = note.text.toLowerCase()
    let score = 0
    let all = true
    for (const term of wanted) {
      const inTitle = count(title, term)
      const inBody = count(body, term)
      if (inTitle + inBody === 0) {
        all = false
        break
      }
      score += inBody + inTitle * TITLE_WEIGHT
    }
    if (all) scored.push({ note, score })
  }
  scored.sort((a, b) => b.score - a.score || (a.note.title ?? '').localeCompare(b.note.title ?? ''))
  return scored.slice(0, limit).map(({ note }) => ({
    note_id: note.id,
    title: note.title,
    snippet: snippet(note.text, wanted),
  }))
}

function count(haystack: string, needle: string): number {
  let n = 0
  for (let i = haystack.indexOf(needle); i !== -1; i = haystack.indexOf(needle, i + needle.length)) n++
  return n
}

/**
 * A window of `text` around its first match, with matching words bracketed — the shape
 * `snippet(notes_fts, 2, '[', ']', '…', 12)` produces on the server, so the pane renders one
 * kind of result.
 */
export function snippet(text: string, wanted: string[]): string {
  const words = text.split(/\s+/u).filter((w) => w.length > 0)
  const hit = (w: string) => wanted.some((t) => w.toLowerCase().includes(t))
  const at = words.findIndex(hit)
  if (at === -1) return words.slice(0, CONTEXT_WORDS * 2).join(' ')
  const from = Math.max(0, at - CONTEXT_WORDS)
  const to = Math.min(words.length, at + CONTEXT_WORDS + 1)
  const body = words.slice(from, to).map((w) => (hit(w) ? `[${w}]` : w)).join(' ')
  return `${from > 0 ? '…' : ''}${body}${to < words.length ? '…' : ''}`
}
