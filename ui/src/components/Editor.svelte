<script lang="ts">
  import { onDestroy, onMount } from 'svelte'
  import { EditorView } from '@codemirror/view'
  import { createEditor, setViewMode, type NoteSource, type ViewMode } from '../lib/editor/setup.ts'
  import { listIndent } from '../lib/editor/lists.ts'
  import { insertLink, run, toggleMark, toggleTask, toggleWikilink, type Plan } from '../lib/editor/format.ts'
  import { MOD } from '../lib/editor/selectionBar.ts'
  import { startCompletion } from '@codemirror/autocomplete'
  import Icon from './Icon.svelte'
  import type { VaultSession } from '../lib/vault.svelte.ts'
  import { api } from '../lib/api.ts'
  import { displayName } from '../lib/vault.svelte.ts'

  import { syntaxTree } from '@codemirror/language'
  import type { OutlineItem } from '../lib/outline.ts'
  import { furnitureHost, pageFurniture, renderPageFoot, renderPageHead, type Backlink } from '../lib/editor/page.ts'
  import { embedUrlFor } from '../lib/attachments.ts'
  import { addTagToFrontMatter, cleanTag } from '../lib/tagedit.ts'
  import ContextMenu, { menuAt, type MenuItem, type MenuState } from './ContextMenu.svelte'
  import { baseName, extension, fileKind, folderOf } from '../lib/filetree.ts'
  import type { PageFile } from '../lib/editor/page.ts'

  let {
    session,
    noteId,
    onOpen,
    onHeadings,
    onHere,
    onTag,
    onTagMenu,
    onAsk,
    onPresence,
    onStats,
    trail = [],
    onOpenFile,
    onUploadFiles,
    mode = 'live',
    jumpTo = $bindable(),
  }: {
    session: VaultSession
    noteId: string
    onOpen: (id: string) => void
    onHeadings?: (items: OutlineItem[]) => void
    /** Where the reader is: the document position at the top of the viewport. */
    onHere?: (pos: number) => void
    /** A tag chip at the foot of the page was clicked: show what else carries it. */
    onTag: (tag: string) => void
    /** Right-click (or long press) on a chip, for the menu the shell builds. */
    onTagMenu?: (tag: string, noteId: string, e: MouseEvent) => void
    /** The shell's prompt dialog, for naming a new tag. */
    onAsk?: (
      title: string,
      initial: string,
      opts?: { placeholder?: string; suggestions?: string[] },
    ) => Promise<string | null>
    onPresence?: (names: string[]) => void
    /** Line and word counts for the pane's footer. Debounced with the outline. */
    onStats?: (stats: { lines: number; words: number }) => void
    /** Where the note lives, folder by folder — drawn on the page above its first line. */
    trail?: string[]
    /** Open one of the note's files in a tab (SPEC §9). Without it there is no Files shelf. */
    onOpenFile?: (vault: string, path: string) => void
    /** Upload into `folder` (`null`: let the dialog ask). */
    onUploadFiles?: (vault: string, folder: string | null) => void
    /** SPEC §8: live preview, plain source, or rendered and read-only. */
    mode?: ViewMode
    jumpTo?: (pos: number) => void
  } = $props()

  let host: HTMLDivElement
  let view: EditorView | undefined
  let release: (() => void) | undefined

  // The page's own furniture: the folder trail above the first line, the tags and backlinks
  // below the last one. Both are nodes we own and CodeMirror merely hosts (lib/editor/page.ts),
  // so they scroll with the note and share its measure without living in the document.
  const head = furnitureHost('cm-page-head')
  const foot = furnitureHost('cm-page-foot')
  let tags: string[] = $state([])
  let backlinks: Backlink[] = $state([])
  $effect(() => renderPageHead(head, trail))
  // The files this note uses, as the file manager lists them; kept current as the note is
  // edited (a new link, a theme named in the front matter) by `filesChanged` below.
  $effect(() => {
    if (onOpenFile) session.watchFiles()
  })
  let files: PageFile[] = $derived(
    session.files
      .filter((f) => f.used_by.includes(noteId))
      .map((f) => ({
        path: f.path,
        name: baseName(f.path),
        thumb: fileKind(f.path) === 'image' ? api.attachmentUrl(session.id, f.hash) : undefined,
        badge: extension(f.path).slice(0, 4).toUpperCase() || 'FILE',
      })),
  )
  let uploadMenu: MenuState | null = $state(null)
  function uploadChoices(e: MouseEvent) {
    const dir = folderOf(session.pathOf(noteId) ?? '')
    uploadMenu = menuAt(e, [
      { label: `Next to this note — ${dir ? `${dir}/` : 'the vault root'}`, run: () => onUploadFiles?.(session.id, dir) },
      { label: 'In attachments/', run: () => onUploadFiles?.(session.id, 'attachments') },
      { label: 'In another folder…', run: () => onUploadFiles?.(session.id, null) },
    ])
  }
  $effect(() =>
    renderPageFoot(foot, {
      files: onOpenFile ? files : undefined,
      onOpenFile: onOpenFile && ((path) => onOpenFile(session.id, path)),
      onUpload: onUploadFiles && uploadChoices,
      tags,
      backlinks,
      onOpen,
      onTag,
      onAddTag: onAsk && addTag,
      onTagMenu: onTagMenu && ((tag, e) => onTagMenu(tag, noteId, e)),
    }),
  )
  // Backlinks are a round trip, so they are fetched once per note rather than per keystroke;
  // a link written elsewhere shows up the next time this note is opened.
  $effect(() => {
    const id = noteId
    const vault = session.id
    let live = true
    backlinks = []
    api
      .backlinks(vault, id)
      .then((rows) => live && (backlinks = rows.map((b) => ({ id: b.id, label: b.title ?? displayName(b.path), path: b.path }))))
      .catch(() => {
        /* no server, or a vault that does not keep an index: the shelf just says nothing */
      })
    return () => {
      live = false
    }
  })

  /**
   * Put another tag on this note. It goes in the front matter rather than into the prose: a tag
   * the reader adds is a declaration *about* the note, and the front matter is the one place it
   * can be taken off again without hunting through the text for a `#word`.
   */
  async function addTag() {
    const v = view
    if (!v || !onAsk) return
    // The vault's own tags, minus the ones this note already has: completing to a tag that is
    // already on the page is the one suggestion that cannot be useful.
    const known = await api
      .tags(session.id)
      .then((t) => t.map((x) => x.tag).filter((t) => !tags.includes(t)))
      .catch(() => [])
    const typed = await onAsk('Add a tag', '', { placeholder: 'name', suggestions: known })
    if (typed === null) return
    const tag = cleanTag(typed)
    // Already carried — inline or declared — so there is nothing to write.
    if (!tag || tags.includes(tag)) return
    const edit = addTagToFrontMatter(v.state.doc.toString(), tag)
    if (edit) v.dispatch({ changes: edit })
    v.focus()
  }

  const embedUrl = (target: string) => embedUrlFor(session, session.pathOf(noteId) ?? '', target)

  /** Where `![[note]]` embeds read from: this vault, live. */
  const notes: NoteSource = {
    resolve: (target) => {
      const hit = session.resolveLink(target)
      return hit && { id: hit.id, title: displayName(hit.path) }
    },
    follow: (id, onText) => session.watchNote(id, onText),
    embedUrl: (id, target) => embedUrlFor(session, session.pathOf(id) ?? '', target),
    watch: (onChange) => session.watchPaths(onChange),
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
    if (!refs.length) return
    // Not glued to the word before it, which would read as part of that word.
    const insert = (/\S/u.test(view.state.sliceDoc(at - 1, at)) ? ' ' : '') + refs.join('\n')
    view.dispatch({ changes: { from: at, insert }, selection: { anchor: at + insert.length } })
  }

  /**
   * A note's tags are the ones the index would find: the inline `#tags` the syntax tree just
   * gave us, then whatever `tags:` its front matter declares — which is where most notes
   * actually keep them, and which the tree does not see at all. Normalised (and ordered)
   * through the indexer's own `pushTag`, so the shelf agrees with the tag pane and search.
   *
   * The front-matter parser arrives lazily: it is a whole YAML parser, and the tag shelf is
   * the only thing in the main bundle that wants one.
   */
  let tagSeq = 0
  async function readTags(doc: string, inline: string[]) {
    const seq = ++tagSeq
    const { frontMatter, pushTag } = await import('../markdown/frontmatter.ts')
    if (seq !== tagSeq) return
    const list: string[] = []
    for (const t of inline) pushTag(list, t)
    for (const t of frontMatter(doc).tags) pushTag(list, t)
    tags = list
  }

  let headingTimer: ReturnType<typeof setTimeout> | undefined
  /** Outline and counts share one debounce: both walk the whole document, and both are read
   *  by chrome outside the editor that has no reason to update mid-keystroke. */
  function reportHeadings(v: EditorView) {
    clearTimeout(headingTimer)
    headingTimer = setTimeout(() => {
      const items: OutlineItem[] = []
      const inline: string[] = []
      syntaxTree(v.state).iterate({
        enter(node) {
          if (node.name === 'NoteTag') {
            inline.push(v.state.sliceDoc(node.from, node.to).trim().replace(/^#/u, ''))
            return
          }
          const m = /^ATXHeading(\d)$/u.exec(node.name)
          if (!m) return
          const text = v.state.sliceDoc(node.from, node.to).replace(/^#+\s*/u, '').trim()
          items.push({ level: Number(m[1]), text, pos: node.from })
        },
      })
      onHeadings?.(items)
      const doc = v.state.doc.toString()
      void readTags(doc, inline)
      const text = doc.trim()
      onStats?.({ lines: v.state.doc.lines, words: text ? text.split(/\s+/u).length : 0 })
    }, 150)
  }
  let hereFrame = 0
  /** Which section the reader is in, for the margin index. Coalesced into a frame: a scroll
   *  fires far more often than the answer to that question changes. */
  function reportHere() {
    if (hereFrame) return
    hereFrame = requestAnimationFrame(() => {
      hereFrame = 0
      if (!view) return
      const box = view.scrollDOM.getBoundingClientRect()
      // Down the middle of the scroller, just below its top edge: the first line still on screen.
      onHere?.(view.posAtCoords({ x: box.left + box.width / 2, y: box.top + 8 }, false))
    })
  }
  const headingWatcher = EditorView.updateListener.of((u) => {
    if (u.docChanged) reportHeadings(u.view)
    // A link or a front-matter path may have changed which files the note uses.
    if (u.docChanged && onOpenFile) session.filesChanged()
    // Headings move when the text does, and when a fold or a widget resizes around them.
    if (u.docChanged || u.geometryChanged) reportHere()
  })

  /** Run a formatting command from a button or the menu, and hand the focus back to the text. */
  function format(plan: Plan) {
    const v = view
    if (!v) return
    run(plan)(v)
    // An empty `[[]]` is only useful with the note names offered inside it.
    if (plan === toggleWikilink && v.state.selection.main.empty) startCompletion(v)
    v.focus()
  }

  let picker: HTMLInputElement
  /** Upload from the device, into the note at the cursor — the menu's and the phone bar's way in. */
  function pickFiles() {
    picker.value = ''
    picker.click()
  }
  function picked() {
    if (view && picker.files?.length) void insertFiles(picker.files, view.state.selection.main.head)
  }

  /** The editor's own right-click menu (below): the format commands and an upload. */
  let textMenu: MenuState | null = $state(null)
  function editorMenu(e: MouseEvent, v: EditorView): boolean {
    // Shift+right-click is the way through to the browser's menu, which is where the
    // spelling suggestions live. A long press on a touch screen is the platform's text
    // selection, which a menu of ours would take away.
    const touch = (e as PointerEvent).pointerType === 'touch' || ((e as PointerEvent).pointerType === undefined && matchMedia('(pointer: coarse)').matches)
    if (e.shiftKey || touch) return false
    // A click outside the selection moves the cursor there first, as the native menu does, so
    // "Insert image" lands where the click was.
    const at = v.posAtCoords({ x: e.clientX, y: e.clientY })
    const sel = v.state.selection.main
    if (at !== null && (at < sel.from || at > sel.to)) v.dispatch({ selection: { anchor: at } })
    const selected = !v.state.selection.main.empty
    const text = () => v.state.sliceDoc(v.state.selection.main.from, v.state.selection.main.to)
    const editable = !v.state.readOnly && v.state.facet(EditorView.editable)
    const clip = typeof navigator.clipboard?.writeText === 'function'
    const items: MenuItem[] = []
    if (clip) {
      if (editable) {
        items.push({
          label: 'Cut',
          hint: `${MOD}X`,
          disabled: !selected,
          run: () => void navigator.clipboard.writeText(text()).then(() => (v.dispatch(v.state.replaceSelection('')), v.focus())),
        })
      }
      items.push({ label: 'Copy', hint: `${MOD}C`, disabled: !selected, run: () => void navigator.clipboard.writeText(text()).then(() => v.focus()) })
      if (editable && typeof navigator.clipboard.readText === 'function') {
        items.push({
          label: 'Paste',
          hint: `${MOD}V`,
          run: () =>
            void navigator.clipboard.readText().then(
              (t) => (v.dispatch(v.state.replaceSelection(t)), v.focus()),
              () => v.focus(),
            ),
        })
      }
    }
    if (editable) {
      if (items.length) items.push({ label: '', separator: true })
      items.push(
        { label: 'Bold', hint: `${MOD}B`, run: () => format(toggleMark('**')) },
        { label: 'Italic', hint: `${MOD}I`, run: () => format(toggleMark('*')) },
        { label: 'Strikethrough', hint: `${MOD}Shift+X`, run: () => format(toggleMark('~~')) },
        { label: 'Code', run: () => format(toggleMark('`')) },
        { label: '', separator: true },
        { label: 'Link', hint: `${MOD}Shift+K`, run: () => format(insertLink) },
        { label: 'Link to a note', run: () => format(toggleWikilink) },
        { label: 'Checklist item', run: () => format(toggleTask) },
        { label: '', separator: true },
        { label: 'Insert image or file…', run: pickFiles },
      )
    }
    if (!items.length) return false
    textMenu = menuAt(e, items)
    return true
  }

  const fileHandlers = EditorView.domEventHandlers({
    contextmenu: editorMenu,
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
      notes,
      noteId,
      mode,
      extra: [fileHandlers, headingWatcher, pageFurniture(head, foot)],
      complete: {
        notes: () => session.notes.map((n) => n.path),
        tags: async () => (await api.tags(session.id).catch(() => [])).map((t) => t.tag),
      },
    })
    view.focus()
    reportHeadings(view)
    view.scrollDOM.addEventListener('scroll', reportHere, { passive: true })
    reportHere()
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
    if (view) setViewMode(view, m, { openLink, embedUrl, notes, noteId })
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
    cancelAnimationFrame(hereFrame)
    view?.destroy()
    release?.()
  })
</script>

{#if uploadMenu}
  <ContextMenu menu={uploadMenu} onClose={() => (uploadMenu = null)} />
{/if}
{#if textMenu}
  <ContextMenu menu={textMenu} onClose={() => (textMenu = null)} />
{/if}
<input class="picker" type="file" multiple bind:this={picker} onchange={picked} tabindex="-1" aria-hidden="true" />

<div class="frame">
  <!-- A phone keyboard has no Tab, no Ctrl for bold, and buries `[` and `*` a layer or two
       down, so the edits that need them get buttons (SPEC §8). The bar sits above the editor
       rather than over the keyboard, so it stays reachable while typing, and scrolls sideways
       when the screen is narrower than it. The press is cancelled on `pointerdown` so the
       editor keeps the focus and the selection the command is about to act on, while the click
       still arrives — which is also what makes the buttons work from the keyboard. -->
  {#if mode !== 'reading'}
    <div class="touchbar" role="toolbar" aria-label="Format">
      <button onpointerdown={keepFocus} onclick={() => indent(-1)} title="Outdent (Shift+Tab)" aria-label="Outdent"><Icon name="outdent" size={16} /></button>
      <button onpointerdown={keepFocus} onclick={() => indent(1)} title="Indent (Tab)" aria-label="Indent"><Icon name="indent" size={16} /></button>
      <button onpointerdown={keepFocus} onclick={() => format(toggleTask)} title="Checklist item" aria-label="Checklist item"><Icon name="task" size={16} /></button>
      <button class="b" onpointerdown={keepFocus} onclick={() => format(toggleMark('**'))} title="Bold" aria-label="Bold">B</button>
      <button class="i" onpointerdown={keepFocus} onclick={() => format(toggleMark('*'))} title="Italic" aria-label="Italic">I</button>
      <button onpointerdown={keepFocus} onclick={() => format(toggleWikilink)} title="Link to a note" aria-label="Link to a note">[[ ]]</button>
      <button onpointerdown={keepFocus} onclick={() => format(insertLink)} title="Link" aria-label="Link">Link</button>
      <button onpointerdown={keepFocus} onclick={pickFiles} title="Insert image or file" aria-label="Insert image or file"><Icon name="attach" size={16} /></button>
    </div>
  {/if}
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
  .picker {
    display: none;
  }
  @media (pointer: coarse) {
    .touchbar {
      display: flex;
      gap: 0.3rem;
      overflow-x: auto;
      scrollbar-width: none;
      padding: 0.3rem 0.5rem;
      border-bottom: 1px solid var(--border);
      background: var(--panel);
    }
    .touchbar button {
      display: flex;
      flex: none;
      align-items: center;
      font: inherit;
      font-size: 0.85rem;
      padding: 0.45rem 0.8rem;
      border: 1px solid var(--border);
      border-radius: 6px;
      background: var(--bg);
      color: inherit;
    }
    .touchbar .b {
      font-weight: 700;
    }
    .touchbar .i {
      font-style: italic;
      font-family: Georgia, serif;
    }
  }
</style>
