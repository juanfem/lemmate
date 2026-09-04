// Tab and Shift-Tab inside a list.
//
// Markdown nests by column: an item becomes a child of the one above it only when its marker
// starts where that item's *content* starts. CodeMirror's generic indent adds one indent unit,
// which is not that column — two spaces under `1. ` leaves the item in the same list, and under
// four it would be a code block — so lists need commands of their own.

import type { Command } from '@codemirror/view'

const ITEM = /^([ \t]*)([-*+]|\d{1,9}[.)])([ \t]+|$)/u
/** What a tab is worth when a line mixes tabs and spaces, as in CommonMark. */
const TAB = 4

/** Display column at the end of `s`, counting a tab as up to the next multiple of four. */
function width(s: string): number {
  let col = 0
  for (const c of s) col = c === '\t' ? col + TAB - (col % TAB) : col + 1
  return col
}

export interface Item {
  /** Column the marker starts at. */
  indent: number
  /** Column the content starts at — where a child's marker has to go. */
  content: number
  /** The written number of an ordered item; `null` for a bullet. */
  number: number | null
  /** Characters of the raw line before the content. */
  prefix: number
}

export function parseItem(line: string): Item | null {
  const m = ITEM.exec(line)
  if (!m) return null
  const [indent, marker, gap] = [m[1]!, m[2]!, m[3]!]
  const num = /^\d/u.test(marker) ? Number(marker.slice(0, -1)) : null
  return {
    indent: width(indent),
    content: width(indent + marker + gap),
    number: num,
    prefix: indent.length + marker.length + gap.length,
  }
}

const isBlank = (line: string) => line.trim() === ''

/** The lines below `i` that belong to its item: everything indented past the marker. */
function blockEnd(lines: string[], i: number, indent: number): number {
  let end = i
  for (let j = i + 1; j < lines.length; j++) {
    if (isBlank(lines[j]!)) continue
    if (width(/^[ \t]*/u.exec(lines[j]!)![0]) <= indent) break
    end = j
  }
  return end
}

/**
 * Where the item on line `i` goes when it is indented (`dir` 1) or outdented (`dir` -1), and
 * what number it takes there — `null` when it cannot move: the first item of a list has nothing
 * to nest under, and a top-level one has nowhere to come out to.
 */
function target(lines: string[], i: number, item: Item, dir: 1 | -1): { indent: number; number: number } | null {
  let indent: number | null = null
  for (let j = i - 1; j >= 0 && indent === null; j--) {
    if (isBlank(lines[j]!)) continue
    const above = parseItem(lines[j]!)
    if (!above) {
      // A paragraph indented past us is some item's continuation; anything else ends the list.
      if (width(/^[ \t]*/u.exec(lines[j]!)![0]) < item.indent) break
      continue
    }
    if (above.indent === item.indent) indent = dir === 1 ? above.content : null
    else if (above.indent < item.indent) indent = dir === 1 ? null : above.indent
    // A deeper item is inside a previous sibling: keep looking for the sibling itself.
  }
  if (indent === null || indent === item.indent) return null
  // The number is the one this item will have among its new siblings; a bullet ignores it.
  let number = 1
  for (let j = i - 1; j >= 0; j--) {
    if (isBlank(lines[j]!)) continue
    const above = parseItem(lines[j]!)
    if (!above) {
      if (width(/^[ \t]*/u.exec(lines[j]!)![0]) < indent) break
      continue
    }
    if (above.indent === indent) {
      number = (above.number ?? 0) + 1
      break
    }
    if (above.indent < indent) break
  }
  return { indent, number }
}

/**
 * The rewritten text of every line a Tab (`dir` 1) or Shift-Tab (`dir` -1) touches, keyed by
 * line index. Items keep their children: everything indented under an item moves with it.
 * Empty when nothing in the range is a list item that can move.
 */
export function planIndent(lines: string[], first: number, last: number, dir: 1 | -1): Map<number, string> {
  const out = new Map<number, string>()
  let done = -1
  for (let i = first; i <= last; i++) {
    if (i <= done) continue // already moved as part of the item above
    const item = parseItem(lines[i]!)
    if (!item) continue
    const to = target(lines, i, item, dir)
    if (!to) continue
    const end = blockEnd(lines, i, item.indent)
    done = end
    // The line is rebuilt rather than shifted, so an ordered item can be renumbered for the
    // level it lands on; a bullet keeps the character it was written with.
    const m = ITEM.exec(lines[i]!)!
    const marker = item.number === null ? m[2]! : `${to.number}${m[2]!.slice(-1)}`
    out.set(i, ' '.repeat(to.indent) + marker + m[3]! + lines[i]!.slice(item.prefix))
    const shift = to.indent - item.indent
    for (let j = i + 1; j <= end; j++) {
      const line = lines[j]!
      if (isBlank(line)) continue
      const lead = /^[ \t]*/u.exec(line)![0]
      out.set(j, ' '.repeat(Math.max(0, width(lead) + shift)) + line.slice(lead.length))
    }
  }
  return out
}

/**
 * Tab / Shift-Tab. Returns false when the selection holds no list item that can move, so the
 * generic indent still handles code blocks, prose, and the first item of a list.
 */
export function listIndent(dir: 1 | -1): Command {
  return (view) => {
    const { state } = view
    const lines = state.doc.toString().split('\n')
    const changes: { from: number; to: number; insert: string }[] = []
    const seen = new Set<number>()
    for (const range of state.selection.ranges) {
      const first = state.doc.lineAt(range.from).number - 1
      const last = state.doc.lineAt(range.to).number - 1
      for (const [i, text] of planIndent(lines, first, last, dir)) {
        if (seen.has(i)) continue
        seen.add(i)
        const line = state.doc.line(i + 1)
        changes.push({ from: line.from, to: line.to, insert: text })
      }
    }
    if (!changes.length) return false
    view.dispatch({ changes, userEvent: 'input.indent' })
    return true
  }
}
