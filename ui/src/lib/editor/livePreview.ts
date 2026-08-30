// Live preview (SPEC §8): the source is always the document; markup is hidden and rendered in
// place, and revealed again on any line the selection touches. Lossless by construction.

import { Decoration, EditorView, ViewPlugin, WidgetType, type DecorationSet, type ViewUpdate } from '@codemirror/view'
import { RangeSetBuilder, type EditorState } from '@codemirror/state'
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

/** Does any selection range touch the lines spanned by [from, to]? */
function revealed(state: EditorState, from: number, to: number): boolean {
  const a = state.doc.lineAt(from).from
  const b = state.doc.lineAt(to).to
  return state.selection.ranges.some((r) => r.from <= b && r.to >= a)
}

function build(view: EditorView, opts: LivePreviewOptions): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>()
  const { state } = view
  const items: { from: number; to: number; deco: Decoration }[] = []
  const push = (from: number, to: number, deco: Decoration) => items.push({ from, to, deco })

  for (const { from, to } of view.visibleRanges) {
    syntaxTree(state).iterate({
      from,
      to,
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
          case 'Blockquote':
            push(node.from, node.from, Decoration.line({ class: 'cm-blockquote' }))
            break
          case 'FencedCode':
            push(node.from, node.from, Decoration.line({ class: 'cm-codeblock-start' }))
            break
          default:
            break
        }
      },
    })
  }
  // RangeSetBuilder requires sorted, non-overlapping input; sort by from then by "line before mark".
  items.sort((a, b) => a.from - b.from || a.to - b.to || (a.deco.spec.class ? -1 : 1))
  let last = -1
  for (const it of items) {
    if (it.from < last) continue // overlapping (nested markup); outer wins
    builder.add(it.from, it.to, it.deco)
    last = Math.max(last, it.to)
  }
  return builder.finish()
}

function hideMarks(n: SyntaxNode, push: (f: number, t: number, d: Decoration) => void) {
  for (const name of ['EmphasisMark', 'CodeMark', 'StrikethroughMark']) {
    for (const m of n.getChildren(name)) push(m.from, m.to, hide)
  }
}

export function livePreview(opts: LivePreviewOptions) {
  return [
    ViewPlugin.fromClass(
      class {
        decorations: DecorationSet
        constructor(view: EditorView) {
          this.decorations = build(view, opts)
        }
        update(u: ViewUpdate) {
          if (u.docChanged || u.viewportChanged || u.selectionSet) this.decorations = build(u.view, opts)
        }
      },
      { decorations: (v) => v.decorations },
    ),
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
