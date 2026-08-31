<script lang="ts">
  import { tick, untrack } from 'svelte'
  import Tree from './Tree.svelte'
  import FolderTree from './FolderTree.svelte'
  import NoteList from './NoteList.svelte'
  import Icon from './Icon.svelte'
  import ContextMenu, { menuAt, type MenuItem, type MenuState } from './ContextMenu.svelte'
  import {
    ancestors,
    buildTree,
    folderKey,
    folderOf,
    folderPaths,
    notesIn,
    rangeBetween,
    visibleNotes,
    type BrowserApi,
    type TreeActions,
    type VaultNode,
  } from '../lib/tree.ts'
  import { canDrop, isInside, type DragPayload } from '../lib/moves.ts'
  import { beginDrag, endDrag, readDrag } from '../lib/dnd.ts'
  import { clamp, dragResize } from '../lib/resize.ts'
  import { displayName } from '../lib/vault.svelte.ts'

  /**
   * The Files sidebar. Two layouts over the same folders:
   *
   * - `tree` — every vault as a root with folders and notes interleaved (the original view);
   * - `split` — folders on top, the selected folder's notes below, after Obsidian's File Tree
   *   Alternative. The bottom list either stops at the folder or reaches into its subfolders.
   *
   * Collapse state, note selection and the drag in flight all live here rather than in the
   * views, because they outlive a switch between them.
   */
  let {
    vaults,
    activeId,
    activeVault,
    onOpen,
    actions = {},
  }: {
    vaults: VaultNode[]
    activeId: string | null
    activeVault?: string | null
    onOpen: (id: string) => void
    actions?: TreeActions
  } = $props()

  type Mode = 'tree' | 'split'
  const FOLDERS_MIN = 80
  const FOLDERS_DEFAULT = 220

  function stored<T>(key: string, fallback: T): T {
    try {
      const raw = localStorage.getItem(key)
      return raw === null ? fallback : (JSON.parse(raw) as T)
    } catch {
      return fallback
    }
  }
  function save(key: string, value: unknown) {
    try {
      localStorage.setItem(key, JSON.stringify(value))
    } catch {
      /* private mode, quota — the view just forgets */
    }
  }

  let mode: Mode = $state(stored<Mode>('lemmate.files.mode', 'tree'))
  // Kept under its original key so existing folds survive the upgrade.
  let collapsed: Record<string, boolean> = $state(stored('lemmate.tree.collapsed', {}))
  let recursive = $state(stored('lemmate.files.recursive', false))
  let foldersHeight = $state(stored('lemmate.files.foldersHeight', FOLDERS_DEFAULT))
  type Selection = { vault: string; folder: string }
  let selected: Selection | null = $state(stored<Selection | null>('lemmate.files.selected', null))
  let host: HTMLElement | undefined = $state()

  // ---- note selection: click replaces, Ctrl/Cmd toggles, Shift takes the range on screen
  let picks: string[] = $state([])
  let anchor: string | null = $state(null)
  let pickSet = $derived(new Set(picks))
  let menu: MenuState | null = $state(null)
  let dropTarget: { vault: string; folder: string } | null = $state(null)

  function setMode(next: Mode) {
    mode = next
    save('lemmate.files.mode', next)
  }
  function toggle(key: string) {
    collapsed[key] = !collapsed[key]
    save('lemmate.tree.collapsed', collapsed)
  }
  function select(vault: string, folder: string) {
    selected = { vault, folder }
    save('lemmate.files.selected', selected)
  }
  function setRecursive(next: boolean) {
    recursive = next
    save('lemmate.files.recursive', next)
  }

  /** Every collapsible key in the sidebar: one per vault, one per folder. */
  function allKeys(): string[] {
    const keys: string[] = []
    for (const v of vaults) {
      keys.push(v.id)
      for (const path of folderPaths(buildTree(v.notes))) keys.push(folderKey(v.id, path))
    }
    return keys
  }
  function expandAll() {
    collapsed = {}
    save('lemmate.tree.collapsed', collapsed)
  }
  function collapseAll() {
    collapsed = Object.fromEntries(allKeys().map((k) => [k, true]))
    save('lemmate.tree.collapsed', collapsed)
  }

  /** Unfold the path down to the open note, select its folder in split mode, scroll to it. */
  async function reveal() {
    const id = activeId
    if (!id) return
    const vault = vaults.find((v) => v.notes.some((n) => n.id === id))
    const note = vault?.notes.find((n) => n.id === id)
    if (!vault || !note) return
    const folder = folderOf(note.path)
    collapsed[vault.id] = false
    for (const a of ancestors(folder)) collapsed[folderKey(vault.id, a)] = false
    save('lemmate.tree.collapsed', collapsed)
    // Only move the selection when the note is not already in the list below — with
    // subfolders included, an ancestor folder is a perfectly good place to have found it.
    if (mode === 'split' && !listed.some((n) => n.id === id)) select(vault.id, folder)
    await tick()
    // The folder pane scrolls on its own: in split mode the note row is in the list below.
    if (mode === 'split') host?.querySelector(`[data-folder="${folderKey(vault.id, folder) || vault.id}"]`)?.scrollIntoView({ block: 'nearest' })
    host?.querySelector(`[data-note="${id}"]`)?.scrollIntoView({ block: 'nearest' })
  }

  // Land on something sensible: the vault the shell is pointing at, and never a vault that
  // has since gone away.
  $effect(() => {
    const fallback = activeVault ?? vaults[0]?.id
    const known = new Set(vaults.map((v) => v.id))
    untrack(() => {
      if (fallback && (!selected || !known.has(selected.vault))) select(fallback, '')
    })
  })

  // A note that was deleted (or moved to another vault) must not stay selected.
  $effect(() => {
    const live = new Set(vaults.flatMap((v) => v.notes.map((n) => n.id)))
    untrack(() => {
      if (picks.some((id) => !live.has(id))) picks = picks.filter((id) => live.has(id))
    })
  })

  let picked = $derived.by(() => {
    const s = selected
    return s ? vaults.find((v) => v.id === s.vault) : undefined
  })
  let pickedRoot = $derived(picked ? buildTree(picked.notes) : null)
  let listed = $derived.by(() => {
    const s = selected
    return pickedRoot && s ? notesIn(pickedRoot, s.folder, recursive) : []
  })
  let listTitle = $derived.by(() => {
    const s = selected
    if (!s) return ''
    return s.folder ? s.folder.slice(s.folder.lastIndexOf('/') + 1) : (picked?.label ?? '')
  })

  /** note id → its vault and path, for drags, menus and the range order. */
  let index = $derived.by(() => {
    const m = new Map<string, { vault: string; path: string }>()
    for (const v of vaults) for (const n of v.notes) m.set(n.id, { vault: v.id, path: n.path })
    return m
  })
  const pathOf = (id: string) => index.get(id)?.path
  const vaultOf = (id: string) => index.get(id)?.vault
  /** What Shift-click ranges over: the rows actually on screen, in the order they are drawn. */
  let order = $derived(mode === 'tree' ? visibleNotes(vaults, collapsed) : listed.map((n) => n.id))

  function noteClick(id: string, e: MouseEvent) {
    if (e.shiftKey && anchor) {
      picks = rangeBetween(order, anchor, id)
      return
    }
    if (e.ctrlKey || e.metaKey) {
      picks = pickSet.has(id) ? picks.filter((x) => x !== id) : [...picks, id]
      anchor = id
      return
    }
    picks = [id]
    anchor = id
    onOpen(id)
  }

  /** The notes a menu or drag on `id` acts on: the whole selection when `id` is part of it. */
  function actOn(id: string): string[] {
    if (pickSet.has(id) && picks.length > 1) return picks
    picks = [id]
    anchor = id
    return [id]
  }

  // ---- drag and drop
  function noteDragStart(id: string, e: DragEvent) {
    const ids = actOn(id)
    const vault = vaultOf(id)
    if (!vault) return
    // A drag carries one vault; a cross-vault selection drags only the part under the pointer.
    beginDrag(e, { vault, notes: ids.filter((n) => vaultOf(n) === vault) })
  }
  function folderDragStart(vault: string, folder: string, e: DragEvent) {
    const v = vaults.find((x) => x.id === vault)
    if (!v) return
    beginDrag(e, { vault, folder, notes: v.notes.filter((n) => isInside(n.path, folder)).map((n) => n.id) })
  }
  function dragOver(vault: string, folder: string, e: DragEvent) {
    const drag = readDrag()
    if (!drag || !canDrop(drag, vault, folder, pathOf)) return
    e.preventDefault()
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move'
    dropTarget = { vault, folder }
  }
  function dragLeave(vault: string, folder: string) {
    if (dropTarget?.vault === vault && dropTarget.folder === folder) dropTarget = null
  }
  function drop(vault: string, folder: string, e: DragEvent) {
    e.preventDefault()
    const drag = readDrag(e)
    dropTarget = null
    endDrag()
    if (drag && canDrop(drag, vault, folder, pathOf)) actions.onMove?.(drag, vault, folder)
  }
  function dragEnd() {
    dropTarget = null
    endDrag()
  }

  // ---- context menus
  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text)
    } catch {
      /* no clipboard permission: the menu entry simply does nothing */
    }
  }

  function noteItems(ids: string[]): MenuItem[] {
    const vault = vaultOf(ids[0]!) ?? ''
    const paths = ids.map((id) => pathOf(id)).filter((p): p is string => p !== undefined)
    if (ids.length > 1) {
      return [
        { label: `${ids.length} notes selected`, disabled: true },
        { label: '', separator: true },
        { label: 'Copy paths', run: () => void copy(paths.join('\n')) },
        { label: '', separator: true },
        { label: `Move ${ids.length} notes to trash`, danger: true, run: () => actions.onTrashNotes?.(vault, ids) },
      ]
    }
    const id = ids[0]!
    const path = paths[0] ?? ''
    return [
      { label: 'Open', run: () => onOpen(id) },
      { label: 'Open in a new pane', run: () => actions.onOpenInPane?.(id), disabled: !actions.onOpenInPane },
      { label: '', separator: true },
      { label: 'Rename / move…', run: () => actions.onRenameNote?.(vault, id) },
      { label: 'Copy path', run: () => void copy(path) },
      { label: 'Copy wikilink', run: () => void copy(`[[${displayName(path)}]]`) },
      { label: '', separator: true },
      { label: 'Bookmark', run: () => actions.onBookmarkNote?.(vault, id) },
      { label: 'Share…', run: () => actions.onShareNote?.(id), disabled: !actions.onShareNote },
      { label: '', separator: true },
      { label: 'Move to trash', danger: true, run: () => actions.onTrashNotes?.(vault, [id]) },
    ]
  }

  function folderItems(vault: string, folder: string): MenuItem[] {
    if (folder === '') {
      return [
        { label: 'New note in this vault', run: () => actions.onCreateInVault?.(vault) },
        { label: '', separator: true },
        { label: 'Rename vault…', run: () => actions.onRenameVault?.(vault) },
        { label: 'Import an Obsidian vault…', run: () => actions.onImportInto?.(vault) },
        { label: '', separator: true },
        { label: 'Expand all', run: expandAll },
        { label: 'Collapse all', run: collapseAll },
        { label: '', separator: true },
        { label: 'New vault…', run: () => actions.onNewVault?.() },
      ]
    }
    return [
      { label: 'New note here', run: () => actions.onCreateIn?.(vault, folder) },
      { label: '', separator: true },
      { label: 'Rename / move folder…', run: () => actions.onRenameFolder?.(vault, folder) },
      { label: 'Copy path', run: () => void copy(folder) },
      { label: '', separator: true },
      { label: 'Move folder to trash', danger: true, run: () => actions.onDeleteFolder?.(vault, folder) },
    ]
  }

  let browser: BrowserApi = $derived({
    selected: pickSet,
    dropTarget,
    onNoteClick: noteClick,
    onNoteMenu: (id, e) => (menu = menuAt(e, noteItems(actOn(id)))),
    onFolderMenu: (vault, folder, e) => (menu = menuAt(e, folderItems(vault, folder))),
    onNoteDragStart: noteDragStart,
    onFolderDragStart: folderDragStart,
    onDragEnd: dragEnd,
    onDragOver: dragOver,
    onDragLeave: dragLeave,
    onDrop: drop,
  })
</script>

<!-- Clicking past the last row clears the selection, the way a file manager does. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div
  class="files"
  class:split={mode === 'split'}
  bind:this={host}
  onclick={(e) => {
    if (e.target === e.currentTarget) picks = []
  }}
>
  <div class="toolbar">
    <div class="modes">
      <button class:on={mode === 'tree'} onclick={() => setMode('tree')} title="Single tree" aria-label="Single tree">
        <Icon name="tree" />
      </button>
      <button class:on={mode === 'split'} onclick={() => setMode('split')} title="Folders and notes" aria-label="Folders and notes">
        <Icon name="split" />
      </button>
    </div>
    <span class="gap">{#if picks.length > 1}<span class="picked">{picks.length} selected</span>{/if}</span>
    <button onclick={expandAll} title="Expand all" aria-label="Expand all"><Icon name="expand" /></button>
    <button onclick={collapseAll} title="Collapse all" aria-label="Collapse all"><Icon name="collapse" /></button>
    <button onclick={reveal} disabled={!activeId} title="Reveal the open note" aria-label="Reveal the open note">
      <Icon name="locate" />
    </button>
  </div>

  {#if mode === 'tree'}
    <Tree {vaults} {activeId} {activeVault} {collapsed} onToggle={toggle} {browser} {actions} />
  {:else}
    <div class="folders-wrap" style:height="{foldersHeight}px">
      <FolderTree {vaults} {selected} {collapsed} onToggle={toggle} onSelect={select} {browser} {actions} />
    </div>
    <!-- A window splitter is a focusable `separator` per ARIA; svelte's rule only knows the
         static kind. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="hsplit"
      role="separator"
      aria-valuenow={foldersHeight}
      aria-orientation="horizontal"
      aria-label="Resize the folder list"
      tabindex="0"
      onpointerdown={(e) =>
        dragResize(e, {
          axis: 'y',
          from: foldersHeight,
          min: FOLDERS_MIN,
          max: Math.max(FOLDERS_MIN, (host?.clientHeight ?? 600) - 160),
          onMove: (v) => (foldersHeight = v),
          onEnd: (v) => save('lemmate.files.foldersHeight', v),
        })}
      onkeydown={(e) => {
        const step = e.key === 'ArrowUp' ? -16 : e.key === 'ArrowDown' ? 16 : 0
        if (!step) return
        e.preventDefault()
        foldersHeight = clamp(foldersHeight + step, FOLDERS_MIN, Math.max(FOLDERS_MIN, (host?.clientHeight ?? 600) - 160))
        save('lemmate.files.foldersHeight', foldersHeight)
      }}
      ondblclick={() => {
        foldersHeight = FOLDERS_DEFAULT
        save('lemmate.files.foldersHeight', foldersHeight)
      }}
    ></div>
    <div class="list-head">
      <span class="title" title={selected?.folder || picked?.label}>{listTitle}</span>
      <span class="n">{listed.length}</span>
      <button
        class:on={recursive}
        onclick={() => setRecursive(!recursive)}
        title={recursive ? 'Showing notes from subfolders too' : 'Showing only this folder'}
        aria-pressed={recursive}
        aria-label="Include notes in subfolders"
      >
        <Icon name="subfolders" />
      </button>
    </div>
    <NoteList
      notes={listed}
      base={selected?.folder ?? ''}
      {activeId}
      showFolders={recursive}
      empty={recursive ? 'No notes in this folder or below.' : 'No notes directly in this folder.'}
      {browser}
    />
  {/if}
</div>

{#if menu}
  <ContextMenu {menu} onClose={() => (menu = null)} />
{/if}

<style>
  .files {
    display: grid;
    grid-template-rows: auto 1fr;
    min-height: 0;
    overflow: hidden;
  }
  /* toolbar · folders (dragged) · splitter · list header · notes */
  .files.split {
    grid-template-rows: auto auto auto auto 1fr;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 0.1rem;
    padding: 0.25rem 0.4rem;
    border-bottom: 1px solid var(--border);
  }
  .gap {
    flex: 1;
    min-width: 0;
    overflow: hidden;
  }
  .picked {
    color: var(--accent);
    font-size: 0.72rem;
    padding-left: 0.4rem;
    white-space: nowrap;
  }
  .modes {
    display: flex;
    gap: 0.1rem;
    margin-right: 0.2rem;
  }
  .toolbar button,
  .list-head button {
    display: grid;
    place-items: center;
    border: 0;
    background: none;
    color: var(--muted);
    padding: 0.25rem;
    border-radius: 4px;
    cursor: pointer;
  }
  .toolbar button:hover:not(:disabled),
  .list-head button:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .toolbar button:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .toolbar button.on,
  .list-head button.on {
    color: var(--accent);
    background: var(--accent-bg);
  }
  .folders-wrap {
    display: grid;
    min-height: 0;
    overflow: hidden;
  }
  /* Grab area wider than the hairline it draws, so the drag is not a pixel hunt. */
  .hsplit {
    height: 7px;
    margin: -3px 0;
    cursor: row-resize;
    background: none;
    position: relative;
    z-index: 1;
  }
  .hsplit::after {
    content: '';
    position: absolute;
    inset: 3px 0;
    background: var(--border);
  }
  .hsplit:hover::after,
  .hsplit:focus-visible::after {
    background: var(--accent);
  }
  .list-head {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.2rem 0.4rem 0.2rem 0.6rem;
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--border) 30%, transparent);
  }
  .list-head .title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.72rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--muted);
  }
  .list-head .n {
    color: var(--muted);
    font-size: 0.72rem;
  }
</style>
