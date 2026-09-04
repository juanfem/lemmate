// Tags are a hierarchy — `#projects/alpha` is under `#projects`, and asking for the parent has
// always answered with the children (SPEC §10, `notes_with_tag`). This turns the flat listing
// the API returns into the shape that fact implies, so the pane can draw it as one.

export interface TagRow {
  tag: string
  count: number
}

export interface TagNode {
  /** The whole tag. This is what a click asks for, and what a rename renames. */
  tag: string
  /** The last segment alone — the rows above supply the rest, so repeating it is noise. */
  name: string
  /** Notes under it, listing included. Absent for a branch the listing did not name. */
  count?: number
  children: TagNode[]
}

/**
 * The listing as a tree. Branch points come from the *names*, not only from the rows: a server
 * that lists `projects/alpha` without listing `projects` still gets a `projects` to hang it on
 * — it just has no count of its own to show, which is honest rather than invented.
 */
export function buildTagTree(rows: TagRow[]): TagNode[] {
  const counts = new Map(rows.map((r) => [r.tag, r.count]))
  const nodes = new Map<string, TagNode>()
  const roots: TagNode[] = []

  function node(tag: string): TagNode {
    const found = nodes.get(tag)
    if (found) return found
    const cut = tag.lastIndexOf('/')
    const made: TagNode = { tag, name: tag.slice(cut + 1), count: counts.get(tag), children: [] }
    nodes.set(tag, made)
    if (cut === -1) roots.push(made)
    else node(tag.slice(0, cut)).children.push(made)
    return made
  }
  for (const r of rows) if (r.tag) node(r.tag)

  // Sorted after the fact rather than by sorting the input: a branch point invented above is
  // appended wherever its first child happened to arrive.
  const sort = (list: TagNode[]) => {
    list.sort((a, b) => a.name.localeCompare(b.name))
    for (const n of list) sort(n.children)
  }
  sort(roots)
  return roots
}

/** Every branch point above `tag`: `a/b/c` is under `a` and `a/b`. */
export function tagAncestors(tag: string): string[] {
  const out: string[] = []
  for (let cut = tag.indexOf('/'); cut !== -1; cut = tag.indexOf('/', cut + 1)) out.push(tag.slice(0, cut))
  return out
}
