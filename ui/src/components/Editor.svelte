<script lang="ts">
  import { onDestroy, onMount } from 'svelte'
  import { EditorView } from '@codemirror/view'
  import { createEditor, setViewMode, type ViewMode } from '../lib/editor/setup.ts'
  import { listIndent } from '../lib/editor/lists.ts'
  import Icon from './Icon.svelte'
  import type { VaultSession } from '../lib/vault.svelte.ts'
  import { api } from '../lib/api.ts'
  import { displayName } from '../lib/vault.svelte.ts'

  import { syntaxTree } from '@codemirror/language'
  import type { OutlineItem } from './OutlinePane.svelte'

  let {
    session,
    noteId,
    onOpen,
    onHeadings,
    onPresence,
    mode = 'live',
    jumpTo = $bindable(),
  }: {
    session: VaultSession
    noteId: string
    onOpen: (id: string) => void
    onHeadings?: (items: OutlineItem[]) => void
    onPresence?: (names: string[]) => void
    /** SPEC §8: live preview, plain source, or rendered and read-only. */
    mode?: ViewMode
    jumpTo?: (pos: number) => void
  } = $props()

  let host: HTMLDivElement
  let view: EditorView | undefined
  let release: (() => void) | undefined

  function embedUrl(target: string): string | undefined {
    const t = target.trim()
    const path = session.pathOf(noteId) ?? ''
    const dir = path.includes('/') ? path.slice(0, path.lastIndexOf('/') + 1) : ''
    const name = t.split('/').pop() ?? t
    for (const candidate of [dir + t, t, `attachments/${name}`]) {
      const hash = session.attachments[candidate]
      if (hash) return api.attachmentUrl(session.id, hash)
    }
    const byName = Object.entries(session.attachments).find(([p]) => p.split('/').pop() === name)
    return byName ? api.attachmentUrl(session.id, byName[1]) : undefined
  }

  function openLink(target: string) {
    const hit = session.resolveLink(target)
    if (hit) onOpen(hit.id)
    else {
      const path = target.endsWith('.md') ? target : `${target}.md`
      onOpen(session.createNote(path, `# ${displayName(path)}\n\n`))
    }
  }

  /** Paste/drop files: upload, then reference them at the cursor (images as embeds). */
  async function insertFiles(files: FileList | File[], at: number) {
    if (!view) return
    const refs: string[] = []
    for (const file of Array.from(files)) {
      try {
        const path = await session.uploadAttachment(file.name, new Uint8Array(await file.arrayBuffer()), file.type)
        const name = path.split('/').pop() ?? path
        refs.push(file.type.startsWith('image/') ? `![[${name}]]` : `[${name}](${path})`)
      } catch (e) {
        refs.push(`<!-- upload failed for ${file.name}: ${String(e)} -->`)
      }
    }
    if (refs.length) view.dispatch({ changes: { from: at, insert: refs.join('\n') }, selection: { anchor: at + refs.join('\n').length } })
  }

  let headingTimer: ReturnType<typeof setTimeout> | undefined
  function reportHeadings(v: EditorView) {
    clearTimeout(headingTimer)
    headingTimer = setTimeout(() => {
      const items: OutlineItem[] = []
      syntaxTree(v.state).iterate({
        enter(node) {
          const m = /^ATXHeading(\d)$/u.exec(node.name)
          if (!m) return
          const text = v.state.sliceDoc(node.from, node.to).replace(/^#+\s*/u, '').trim()
          items.push({ level: Number(m[1]), text, pos: node.from })
        },
      })
      onHeadings?.(items)
    }, 150)
  }
  const headingWatcher = EditorView.updateListener.of((u) => {
    if (u.docChanged) reportHeadings(u.view)
  })

  const fileHandlers = EditorView.domEventHandlers({
    paste(event, v) {
      const files = event.clipboardData?.files
      if (!files || files.length === 0) return false
      event.preventDefault()
      void insertFiles(files, v.state.selection.main.head)
      return true
    },
    drop(event, v) {
      const files = event.dataTransfer?.files
      if (!files || files.length === 0) return false
      event.preventDefault()
      const pos = v.posAtCoords({ x: event.clientX, y: event.clientY }) ?? v.state.selection.main.head
      void insertFiles(files, pos)
      return true
    },
  })

  onMount(() => {
    const acquired = session.acquire(noteId)
    release = acquired.release
    const me = localStorage.getItem('lemmate.user') ?? (window as unknown as { lemmate?: { userName?: string } }).lemmate?.userName ?? 'me'
    const hue = [...me].reduce((h, c) => (h * 31 + c.charCodeAt(0)) % 360, 7)
    acquired.awareness.setLocalStateField('user', { name: me, color: `hsl(${hue} 70% 45%)`, colorLight: `hsl(${hue} 70% 45% / 0.25)` })
    const presence = () => {
      const names: string[] = []
      acquired.awareness.getStates().forEach((st, clientId) => {
        if (clientId === acquired.doc.clientID) return
        const u = (st as { user?: { name?: string } }).user
        if (u?.name) names.push(u.name)
      })
      onPresence?.(names)
    }
    acquired.awareness.on('change', presence)
    presence()
    view = createEditor(host, acquired.doc.getText('content'), acquired.awareness, {
      openLink,
      embedUrl,
      mode,
      extra: [fileHandlers, headingWatcher],
      complete: {
        notes: () => session.notes.map((n) => n.path),
        tags: async () => (await api.tags(session.id).catch(() => [])).map((t) => t.tag),
      },
    })
    view.focus()
    reportHeadings(view)
    // If the doc was still empty (not yet synced), move the cursor past the front matter once
    // the content lands, so it opens folded rather than revealed by a cursor stuck at 0.
    const ytext = acquired.doc.getText('content')
    if (ytext.length === 0) {
      const once = () => {
        ytext.unobserve(once)
        const v = view
        if (!v || v.state.selection.main.head !== 0) return
        const text = v.state.doc.toString()
        if (!text.startsWith('---\n')) return
        const close = text.indexOf('\n---', 4)
        if (close === -1) return
        const pos = Math.min(text.length, close + 4 + (text[close + 4] === '\n' ? 1 : 0))
        v.dispatch({ selection: { anchor: pos } })
      }
      ytext.observe(once)
    }
    jumpTo = (pos: number) => {
      if (!view) return
      view.dispatch({ selection: { anchor: pos }, effects: EditorView.scrollIntoView(pos, { y: 'start', yMargin: 20 }) })
      view.focus()
    }
  })

  // Switching mode reconfigures the running view rather than rebuilding it, so the scroll
  // position, the undo history and the collaborative binding all survive the switch.
  $effect(() => {
    const m = mode
    if (view) setViewMode(view, m, { openLink, embedUrl })
  })

  /** A press on the bar must not take the focus off the text it is about to indent. */
  const keepFocus = (e: PointerEvent) => e.preventDefault()

  /** The touch bar's buttons: the same commands Tab and Shift+Tab run. */
  function indent(dir: 1 | -1) {
    if (!view) return
    listIndent(dir)(view)
    view.focus()
  }

  onDestroy(() => {
    view?.destroy()
    release?.()
  })
</script>

<div class="frame">
  <!-- A phone keyboard has no Tab, and nesting a list item is the one edit that needs one. The
       bar sits above the editor rather than over the keyboard, so it stays reachable while
       typing. The press is cancelled on `pointerdown` so the editor keeps the focus and the
       selection the command is about to act on, while the click still arrives — which is also
       what makes the buttons work from the keyboard. -->
  <div class="touchbar">
    <button onpointerdown={keepFocus} onclick={() => indent(-1)} title="Outdent (Shift+Tab)" aria-label="Outdent"><Icon name="outdent" size={16} /></button>
    <button onpointerdown={keepFocus} onclick={() => indent(1)} title="Indent (Tab)" aria-label="Indent"><Icon name="indent" size={16} /></button>
  </div>
  <div class="editor" bind:this={host}></div>
</div>

<style>
  .frame {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .editor {
    flex: 1;
    min-height: 0;
    overflow: auto;
  }
  /* Pointer, not width: a tablet in landscape is wide and still has no Tab key, and a narrow
     window on a laptop has one. */
  .touchbar {
    display: none;
  }
  @media (pointer: coarse) {
    .touchbar {
      display: flex;
      gap: 0.3rem;
      padding: 0.3rem 0.5rem;
      border-bottom: 1px solid var(--border);
      background: var(--panel);
    }
    .touchbar button {
      display: flex;
      align-items: center;
      padding: 0.45rem 0.9rem;
      border: 1px solid var(--border);
      border-radius: 6px;
      background: var(--bg);
      color: inherit;
    }
  }
</style>
