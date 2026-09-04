// The note's own furniture, on the page rather than in a bar beside it: the folder trail above
// the first line, and the tags and backlinks below the last one. Both ride in block widgets so
// they sit inside `.cm-content` — which means they take the measure, the paper and the scroll
// of the document they describe, instead of being a panel that happens to be next to it.
import { Decoration, EditorView, WidgetType, type DecorationSet } from '@codemirror/view'
import { StateField, type EditorState } from '@codemirror/state'

/**
 * A widget that renders a node the *caller* owns and keeps updating. `eq` compares identity so
 * CodeMirror never swaps the node out from under whoever is writing to it, and `destroy` is
 * deliberately empty for the same reason: the node outlives any one decoration.
 */
class HostWidget extends WidgetType {
  readonly host: HTMLElement
  constructor(host: HTMLElement) {
    super()
    this.host = host
  }
  eq(other: HostWidget) {
    return other.host === this.host
  }
  toDOM() {
    return this.host
  }
  ignoreEvent() {
    return true
  }
  destroy() {
    /* the caller owns the node */
  }
}

/** Pins `head` above the first line and `foot` after the last one. */
export function pageFurniture(head: HTMLElement, foot: HTMLElement) {
  const build = (state: EditorState): DecorationSet =>
    Decoration.set(
      [
        Decoration.widget({ widget: new HostWidget(head), block: true, side: -1 }).range(0),
        Decoration.widget({ widget: new HostWidget(foot), block: true, side: 1 }).range(state.doc.length),
      ],
      true,
    )
  return StateField.define<DecorationSet>({
    create: build,
    // Only the foot moves, and only when the document's length does.
    update: (deco, tr) => (tr.docChanged ? build(tr.state) : deco),
    provide: (f) => EditorView.decorations.from(f),
  })
}

/** A node that CodeMirror will host: outside the editable text, and not a drop target for it. */
export function furnitureHost(cls: string): HTMLElement {
  const el = document.createElement('div')
  el.className = cls
  el.contentEditable = 'false'
  el.spellcheck = false
  return el
}

export interface Backlink {
  id: string
  label: string
  path: string
}

/** `notes / Projects` — where the note lives. Its name is the heading right underneath. */
export function renderPageHead(el: HTMLElement, trail: string[]) {
  el.replaceChildren()
  if (trail.length === 0) return
  trail.forEach((part, i) => {
    if (i > 0) {
      const sep = el.appendChild(document.createElement('span'))
      sep.className = 'cm-page-sep'
      sep.textContent = '/'
    }
    el.appendChild(document.createElement('span')).textContent = part
  })
}

/**
 * Tags and backlinks, as two labelled shelves at the foot of the page. This is what the note
 * panel used to hold; a reader wants it *after* the note, not beside it, and the note that
 * links here is worth a click from where you finished reading.
 */
export function renderPageFoot(
  el: HTMLElement,
  data: {
    tags: string[]
    backlinks: Backlink[]
    onOpen: (id: string) => void
    /** Show every note carrying this tag. */
    onTag: (tag: string) => void
    /** Put another tag on this note. Absent where nothing can ask for its name. */
    onAddTag?: () => void
  },
) {
  el.replaceChildren()

  const tags = shelf(el, 'Tags')
  const row = tags.appendChild(document.createElement('div'))
  row.className = 'cm-page-tags'
  // The names arrive normalised, without the `#` an inline tag is written with. Both kinds are
  // drawn the same way: where a note declared its tags is not what the shelf is about. A tag on
  // a note is a question — what else is filed under this — so each chip asks it.
  for (const t of data.tags) {
    const chip = row.appendChild(document.createElement('button'))
    chip.className = 'cm-page-tag'
    chip.type = 'button'
    chip.title = `Show notes tagged #${t}`
    chip.addEventListener('click', () => data.onTag(t))
    chip.textContent = `#${t}`
  }
  if (data.onAddTag) {
    const add = row.appendChild(document.createElement('button'))
    add.className = 'cm-page-tag cm-page-tag-add'
    add.type = 'button'
    add.title = 'Add a tag to this note'
    add.setAttribute('aria-label', 'Add a tag')
    // On an empty shelf a lone `+` has nothing beside it to be a plus *of*, so it says so.
    add.textContent = data.tags.length ? '+' : '+ Add a tag'
    add.addEventListener('click', () => data.onAddTag?.())
  }
  if (data.tags.length === 0) {
    const none = tags.appendChild(document.createElement('p'))
    none.className = 'cm-page-none'
    none.append('Or write ')
    const code = none.appendChild(document.createElement('code'))
    code.textContent = '#a-tag'
    none.append(' in the note, or list ')
    const fm = none.appendChild(document.createElement('code'))
    fm.textContent = 'tags:'
    none.append(' in its front matter.')
  }

  const links = shelf(el, data.backlinks.length ? `Backlinks · ${data.backlinks.length}` : 'Backlinks')
  if (data.backlinks.length === 0) {
    const none = links.appendChild(document.createElement('p'))
    none.className = 'cm-page-none'
    none.textContent = 'Nothing links here yet.'
    return
  }
  for (const b of data.backlinks) {
    const link = links.appendChild(document.createElement('button'))
    link.className = 'cm-page-backlink'
    link.type = 'button'
    link.title = b.path
    link.addEventListener('click', () => data.onOpen(b.id))
    const name = link.appendChild(document.createElement('span'))
    name.className = 'cm-page-backlink-name'
    name.textContent = b.label
    const where = link.appendChild(document.createElement('span'))
    where.className = 'cm-page-backlink-where'
    where.textContent = b.path
  }
}

function shelf(parent: HTMLElement, title: string): HTMLElement {
  const section = parent.appendChild(document.createElement('section'))
  section.className = 'cm-page-shelf'
  const h = section.appendChild(document.createElement('h2'))
  h.className = 'cm-page-shelf-title'
  h.textContent = title
  return section
}
