/**
 * Which lines of `before` are not in `after`, by a plain longest-common-subsequence over whole
 * lines. Version history only ever needs "did this line survive" — a word-level diff would say
 * more than a reader of an old version wants, and the whole point of the page is to read it.
 */
export function changedLines(before: string, after: string): Set<number> {
  // An empty version has no lines to mark. Splitting it would give one empty line that the
  // note does not have, and the whole document would come back "changed" with nothing in it.
  if (before === '') return new Set()
  const a = before.split('\n')
  const b = after.split('\n')
  // Shared head and tail first: an edit in the middle of a long note is otherwise an O(n²)
  // table over lines that all match.
  let head = 0
  while (head < a.length && head < b.length && a[head] === b[head]) head++
  let tail = 0
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail++
  const mid = a.slice(head, a.length - tail)
  const other = b.slice(head, b.length - tail)
  const changed = new Set<number>()
  // A cap rather than a heuristic: the table is |mid| × |other| cells, and beyond this the
  // honest answer ("the middle changed") is as useful as the exact one and arrives at once.
  if (mid.length * other.length > 4_000_000) {
    for (let i = 0; i < mid.length; i++) changed.add(head + i)
    return changed
  }
  const rows = mid.length
  const cols = other.length
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
      i++
      j++
    } else if (lcs[(i + 1) * (cols + 1) + j]! >= lcs[i * (cols + 1) + j + 1]!) {
      changed.add(head + i)
      i++
    } else {
      j++
    }
  }
  for (; i < rows; i++) changed.add(head + i)
  return changed
}
