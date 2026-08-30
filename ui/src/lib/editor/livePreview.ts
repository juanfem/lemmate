// Live preview (SPEC §8): the source is always the document; markup is hidden and rendered in
// place, and revealed again on any line the selection touches. Lossless by construction.

import { Decoration, EditorView, WidgetType, type DecorationSet } from '@codemirror/view'
import { StateField, type EditorState } from '@codemirror/state'
import { syntaxTree } from '@codemirror/language'
import type { SyntaxNode } from '@lezer/common'
import katex from 'katex'

export interface LivePreviewOptions {
  /** Called when a wikilink widget is activated. */
  openLink: (target: string) => void
  /** Resolve an embed target to a URL (attachments) or undefined. */
  embedUrl: (target: string) => string | undefined
}

class MathWidget extends WidgetType {
  readonly tex: string
  readonly display: boolean
  constructor(tex: string, display: boolean) {
    super()
    this.tex = tex
    this.display = display
  }
  eq(other: MathWidget) {
    return other.tex === this.tex && other.display === this.display
  }
  toDOM() {
    const el = document.createElement(this.display ? 'div' : 'span')
    el.className = this.display ? 'cm-math-block' : 'cm-math-inline'
    try {
      katex.render(this.tex, el, { displayMode: this.display, throwOnError: false })
    } catch {
      el.textContent = this.tex
    }
    return el
  }
  ignoreEvent() {
    return false
  }
}

class LinkWidget extends WidgetType {
  readonly label: string
  readonly target: string
  readonly open: (t: string) => void
  constructor(label: string, target: string, open: (t: string) => void) {
    super()
    this.label = label
    this.target = target
    this.open = open
  }
  eq(other: LinkWidget) {
    return other.label === this.label && other.target === this.target
  }
  toDOM() {
    const a = document.createElement('a')
    a.className = 'cm-wikilink'
    a.textContent = this.label
    a.href = '#'
    a.onclick = (e) => {
      e.preventDefault()
      this.open(this.target)
    }
    return a
  }
  ignoreEvent(e: Event) {
    return e.type === 'click'
  }
}

class ImageWidget extends WidgetType {
  readonly url: string
  readonly alt: string
  constructor(url: string, alt: string) {
    super()
    this.url = url
    this.alt = alt
  }
  eq(other: ImageWidget) {
    return other.url === this.url
  }
  toDOM() {
    const img = document.createElement('img')
    img.className = 'cm-embed-image'
    img.src = this.url
    img.alt = this.alt
    return img
  }
}

class CheckboxWidget extends WidgetType {
  readonly checked: boolean
  constructor(checked: boolean) {
    super()
    this.checked = checked
  }
  eq(other: CheckboxWidget) {
    return other.checked === this.checked
  }
  toDOM() {
    const box = document.createElement('input')
    box.type = 'checkbox'
    box.checked = this.checked
    box.className = 'cm-task-checkbox'
    return box
  }
  ignoreEvent() {
    return false
  }
}

const hide = Decoration.replace({})

/** Collapsed front matter: a one-line summary of the properties (SPEC §8). */
class FrontMatterWidget extends WidgetType {
  readonly summary: string
  constructor(summary: string) {
    super()
    this.summary = summary
  }
  eq(other: FrontMatterWidget) {
    return other.summary === this.summary
  }
  toDOM() {
    const el = document.createElement('div')
    el.className = 'cm-frontmatter'
    el.textContent = this.summary || 'properties'
    el.title = 'Front matter — click to edit'
    return el
  }
  ignoreEvent() {
    return false
  }
}

function frontMatterRange(state: EditorState): { from: number; to: number; body: string } | null {
  const first = state.doc.line(1)
  if (first.text.trim() !== '---') return null
  for (let n = 2; n <= Math.min(state.doc.lines, 200); n++) {
    const line = state.doc.line(n)
    if (line.text.trim() === '---' || line.text.trim() === '...') {
      return { from: first.from, to: line.to, body: state.sliceDoc(first.to + 1, line.from) }
    }
  }
  return null
}

function frontMatterSummary(body: string): string {
  const keys: string[] = []
  for (const line of body.split('\n')) {
    const m = /^([A-Za-z_][\w-]*):\s*(.*)$/u.exec(line)
    if (!m) continue
    const v = m[2]!.trim()
    keys.push(v && m[1] !== 'id' ? `${m[1]}: ${v.length > 40 ? v.slice(0, 40) + '…' : v}` : m[1]!)
  }
  return keys.join('   ·   ')
}

/** Does any selection range touch the lines spanned by [from, to]? */
function revealed(state: EditorState, from: number, to: number): boolean {
  const a = state.doc.lineAt(from).from
  const b = state.doc.lineAt(to).to
  return state.selection.ranges.some((r) => r.from <= b && r.to >= a)
}

function build(state: EditorState, opts: LivePreviewOptions): DecorationSet {
  const items: { from: number; to: number; deco: Decoration }[] = []
  const push = (from: number, to: number, deco: Decoration) => items.push({ from, to, deco })

  const fm = frontMatterRange(state)
  if (fm && !revealed(state, fm.from, fm.to)) {
    push(fm.from, fm.to, Decoration.replace({ widget: new FrontMatterWidget(frontMatterSummary(fm.body)), block: true }))
  }

  {
    syntaxTree(state).iterate({
      enter: (node) => {
        const n = node.node
        switch (node.name) {
          case 'ATXHeading1':
          case 'ATXHeading2':
          case 'ATXHeading3':
          case 'ATXHeading4':
          case 'ATXHeading5':
          case 'ATXHeading6': {
            const level = node.name.slice(-1)
            push(node.from, node.from, Decoration.line({ class: `cm-heading cm-h${level}` }))
            if (!revealed(state, node.from, node.to)) {
              const mark = n.getChild('HeaderMark')
              if (mark) push(mark.from, Math.min(mark.to + 1, node.to), hide)
            }
            break
          }
          case 'Emphasis':
          case 'StrongEmphasis':
          case 'Strikethrough':
          case 'InlineCode':
            if (!revealed(state, node.from, node.to)) hideMarks(n, push)
            break
          case 'Link': {
            if (revealed(state, node.from, node.to)) break
            // [text](url "title") → keep text, hide the rest
            const marks = n.getChildren('LinkMark')
            const url = n.getChild('URL')
            if (marks.length >= 2 && url) {
              push(marks[0]!.from, marks[0]!.to, hide)
              push(marks[1]!.from, node.to, hide)
            }
            break
          }
          case 'Image': {
            if (revealed(state, node.from, node.to)) break
            const url = n.getChild('URL')
            if (url) {
              const target = state.sliceDoc(url.from, url.to)
              const src = opts.embedUrl(target) ?? target
              push(node.from, node.to, Decoration.replace({ widget: new ImageWidget(src, ''), block: false }))
            }
            break
          }
          case 'WikiLink': {
            const text = state.sliceDoc(node.from + 2, node.to - 2)
            const [targetPart, label] = text.split('|', 2)
            const target = targetPart!.split('#')[0]!.trim()
            if (!revealed(state, node.from, node.to)) {
              push(node.from, node.to, Decoration.replace({ widget: new LinkWidget((label ?? targetPart!).trim(), target, opts.openLink) }))
            } else {
              push(node.from, node.to, Decoration.mark({ class: 'cm-wikilink-src' }))
            }
            break
          }
          case 'WikiEmbed': {
            const text = state.sliceDoc(node.from + 3, node.to - 2)
            const target = text.split('|')[0]!.trim()
            const url = opts.embedUrl(target)
            if (url && !revealed(state, node.from, node.to)) {
              push(node.from, node.to, Decoration.replace({ widget: new ImageWidget(url, target) }))
            } else if (!revealed(state, node.from, node.to)) {
              push(node.from, node.to, Decoration.replace({ widget: new LinkWidget(`![[${target}]]`, target, opts.openLink) }))
            }
            break
          }
          case 'NoteTag':
            push(node.from, node.to, Decoration.mark({ class: 'cm-tag' }))
            break
          case 'InlineMath': {
            if (revealed(state, node.from, node.to)) break
            const tex = state.sliceDoc(node.from + 1, node.to - 1)
            push(node.from, node.to, Decoration.replace({ widget: new MathWidget(tex, false) }))
            break
          }
          case 'BlockMath': {
            if (revealed(state, node.from, node.to)) break
            const raw = state.sliceDoc(node.from, node.to).trim()
            const tex = raw.replace(/^\$\$/u, '').replace(/\$\$$/u, '').trim()
            push(node.from, node.to, Decoration.replace({ widget: new MathWidget(tex, true), block: true }))
            break
          }
          case 'TaskMarker': {
            if (revealed(state, node.from, node.to)) break
            const checked = /x/iu.test(state.sliceDoc(node.from, node.to))
            push(node.from, node.to, Decoration.replace({ widget: new CheckboxWidget(checked) }))
            break
          }
          case 'Blockquote': {
            // One line class per line of the quote; hide the `> ` markers unless revealed.
            const fromLine = state.doc.lineAt(node.from).number
            const toLine = state.doc.lineAt(node.to).number
            for (let ln = fromLine; ln <= toLine; ln++) push(state.doc.line(ln).from, state.doc.line(ln).from, Decoration.line({ class: 'cm-blockquote' }))
            if (!revealed(state, node.from, node.to)) {
              for (const m of n.getChildren('QuoteMark')) push(m.from, Math.min(m.to + 1, state.doc.lineAt(m.from).to), hide)
            }
            break
          }
          case 'FencedCode':
            push(node.from, node.from, Decoration.line({ class: 'cm-codeblock-start' }))
            break
          default:
            break
        }
      },
    })
  }
  // Replacements may not overlap each other (nested markup: the outer one wins); marks and
  // line decorations may. `Decoration.set(…, true)` sorts by position and side for us.
  items.sort((a, b) => a.from - b.from || b.to - a.to)
  const ranges = []
  let replacedUntil = -1
  for (const it of items) {
    const isReplace = it.deco.spec.widget !== undefined || it.deco.spec.block !== undefined || it.deco === hide
    if (isReplace) {
      if (it.from < replacedUntil) continue
      replacedUntil = Math.max(replacedUntil, it.to)
    }
    ranges.push(it.deco.range(it.from, it.to))
  }
  return Decoration.set(ranges, true)
}

function hideMarks(n: SyntaxNode, push: (f: number, t: number, d: Decoration) => void) {
  for (const name of ['EmphasisMark', 'CodeMark', 'StrikethroughMark']) {
    for (const m of n.getChildren(name)) push(m.from, m.to, hide)
  }
}

export function livePreview(opts: LivePreviewOptions) {
  // A StateField rather than a ViewPlugin: block-level replacements (math blocks, folded
  // front matter) are only allowed from fields. Recomputed on document or selection changes.
  const field = StateField.define<DecorationSet>({
    create: (state) => build(state, opts),
    update: (deco, tr) => (tr.docChanged || tr.selection ? build(tr.state, opts) : deco),
    provide: (f) => EditorView.decorations.from(f),
  })
  return [
    field,
    // Toggle task checkboxes by clicking the rendered box.
    EditorView.domEventHandlers({
      mousedown(event, view) {
        const target = event.target as HTMLElement
        if (!target.classList?.contains('cm-task-checkbox')) return false
        const pos = view.posAtDOM(target)
        const line = view.state.doc.lineAt(pos)
        const m = /^(\s*(?:[-*+]|\d+[.)])\s+\[)( |x|X)(\])/u.exec(line.text)
        if (!m) return false
        const at = line.from + m[1]!.length
        view.dispatch({ changes: { from: at, to: at + 1, insert: m[2] === ' ' ? 'x' : ' ' } })
        event.preventDefault()
        return true
      },
    }),
  ]
}
