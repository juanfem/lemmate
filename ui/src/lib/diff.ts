/**
 * Line diffs for version history: a plain longest-common-subsequence over whole lines. A note is
 * read line by line, and a history page wants "which lines went, which came" — the one place
 * finer grain helps (a word changed in a long line) is handled by {@link inlineSpan} after the
 * fact, on lines already paired up.
 */

export interface DiffLine {
  kind: 'same' | 'add' | 'del'
  text: string
  /** Index of the line in `before` (same, del) and in `after` (same, add). */
  a?: number
  b?: number
}

/** A run of lines the two sides share, or one change: what went, then what came in its place. */
export type Block = { type: 'same'; lines: DiffLine[] } | { type: 'change'; del: DiffLine[]; add: DiffLine[] }

const lines = (text: string) => (text === '' ? [] : text.split('\n'))

/** Every line of both texts, in order, marked by which side it is on. */
export function lineDiff(before: string, after: string): DiffLine[] {
  const a = lines(before)
  const b = lines(after)
  const out: DiffLine[] = []
  // Shared head and tail first: an edit in the middle of a long note is otherwise an O(n²)
  // table over lines that all match.
  let head = 0
  while (head < a.length && head < b.length && a[head] === b[head]) head++
  let tail = 0
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail++
  for (let i = 0; i < head; i++) out.push({ kind: 'same', text: a[i]!, a: i, b: i })

  const mid = a.slice(head, a.length - tail)
  const other = b.slice(head, b.length - tail)
  const rows = mid.length
  const cols = other.length
  // Removals and additions collect here and leave in that order at the next shared line, so a
  // rewritten paragraph reads as the old one, then the new one — not the two interleaved.
  let dels: DiffLine[] = []
  let adds: DiffLine[] = []
  const flush = () => {
    out.push(...dels, ...adds)
    dels = []
    adds = []
  }
  const del = (i: number) => dels.push({ kind: 'del', text: mid[i]!, a: head + i })
  const add = (j: number) => adds.push({ kind: 'add', text: other[j]!, b: head + j })
  // A cap rather than a heuristic: the table is |mid| × |other| cells, and beyond this the
  // honest answer ("the middle changed") is as useful as the exact one and arrives at once.
  if (rows * cols > 4_000_000) {
    for (let i = 0; i < rows; i++) del(i)
    for (let j = 0; j < cols; j++) add(j)
  } else {
    const lcs = new Uint32Array((rows + 1) * (cols + 1))
    for (let i = rows - 1; i >= 0; i--) {
      for (let j = cols - 1; j >= 0; j--) {
        lcs[i * (cols + 1) + j] =
          mid[i] === other[j]
            ? lcs[(i + 1) * (cols + 1) + j + 1]! + 1
            : Math.max(lcs[(i + 1) * (cols + 1) + j]!, lcs[i * (cols + 1) + j + 1]!)
      }
    }
    let i = 0
    let j = 0
    while (i < rows && j < cols) {
      if (mid[i] === other[j]) {
        flush()
        out.push({ kind: 'same', text: mid[i]!, a: head + i, b: head + j })
        i++
        j++
      } else if (lcs[(i + 1) * (cols + 1) + j]! >= lcs[i * (cols + 1) + j + 1]!) {
        del(i++)
      } else {
        add(j++)
      }
    }
    for (; i < rows; i++) del(i)
    for (; j < cols; j++) add(j)
  }
  flush()
  for (let k = tail; k > 0; k--) out.push({ kind: 'same', text: a[a.length - k]!, a: a.length - k, b: b.length - k })
  return out
}

/**
 * Which lines of `before` are not in `after`. Version history's reading view marks these: an
 * old version, with what the note has since lost.
 */
export function changedLines(before: string, after: string): Set<number> {
  const changed = new Set<number>()
  for (const l of lineDiff(before, after)) if (l.kind === 'del') changed.add(l.a!)
  return changed
}

/** The diff as alternating shared runs and changes. */
export function blocks(diff: DiffLine[]): Block[] {
  const out: Block[] = []
  for (const l of diff) {
    const last = out[out.length - 1]
    if (l.kind === 'same') {
      if (last?.type === 'same') last.lines.push(l)
      else out.push({ type: 'same', lines: [l] })
    } else {
      let change = last
      if (change?.type !== 'change') {
        change = { type: 'change', del: [], add: [] }
        out.push(change)
      }
      ;(l.kind === 'del' ? change.del : change.add).push(l)
    }
  }
  return out
}

/**
 * Where two versions of one line differ, as `[start, endBefore, endAfter]`: everything before
 * `start` and from each end on is shared. `null` when the lines share too little for marking
 * the difference to help — then the whole line is the change.
 */
export function inlineSpan(before: string, after: string): [number, number, number] | null {
  let start = 0
  const max = Math.min(before.length, after.length)
  while (start < max && before[start] === after[start]) start++
  let end = 0
  while (end < max - start && before[before.length - 1 - end] === after[after.length - 1 - end]) end++
  if (start + end < Math.max(before.length, after.length) / 4) return null
  // Whole words: a span that starts or ends mid-word is harder to read than a slightly wider one.
  const w = (s: string, i: number) => i >= 0 && i < s.length && /[\p{L}\p{N}_]/u.test(s[i]!)
  while (start > 0 && w(before, start - 1) && (w(before, start) || w(after, start))) start--
  while (end > 0 && w(before, before.length - end) && (w(before, before.length - end - 1) || w(after, after.length - end - 1))) end--
  return [start, before.length - end, after.length - end]
}
