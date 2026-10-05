// Rewriting a note's text in place, as a machine edit: a renamed link, a tag taken off.

import type * as Y from 'yjs'

/**
 * Make `text` read `next`, replacing only the span that actually differs. A whole-document
 * delete-and-insert puts the entire note through the update log, drops everyone else's cursor
 * to the top, and — merged with an edit someone makes meanwhile — duplicates or loses their
 * text; the edit is usually a handful of characters. Returns whether anything changed.
 */
export function replaceText(text: Y.Text, next: string): boolean {
  const before = text.toString()
  if (before === next) return false
  let head = 0
  while (head < before.length && head < next.length && before[head] === next[head]) head++
  let tail = 0
  while (
    tail < before.length - head &&
    tail < next.length - head &&
    before[before.length - 1 - tail] === next[next.length - 1 - tail]
  )
    tail++
  // Never cut between the halves of a surrogate pair (an emoji): yjs mends a split pair with
  // U+FFFD, so the cut moves out to the whole character.
  const high = (s: string, i: number) => i >= 0 && i < s.length && /[\uD800-\uDBFF]/u.test(s[i]!)
  if (head > 0 && high(before, head - 1)) head--
  if (tail > 0 && (high(before, before.length - tail - 1) || high(next, next.length - tail - 1))) tail--
  const apply = () => {
    if (before.length - head - tail > 0) text.delete(head, before.length - head - tail)
    if (next.length - tail > head) text.insert(head, next.slice(head, next.length - tail))
  }
  if (text.doc) text.doc.transact(apply)
  else apply()
  return true
}
