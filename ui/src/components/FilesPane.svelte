<script lang="ts">
  import { tick, untrack } from 'svelte'
  import Tree from './Tree.svelte'
  import FolderTree from './FolderTree.svelte'
  import NoteList from './NoteList.svelte'
  import Icon from './Icon.svelte'
  import { ancestors, buildTree, folderKey, folderOf, folderPaths, notesIn, type TreeActions, type VaultNode } from '../lib/tree.ts'
  import { clamp, dragResize } from '../lib/resize.ts'

  /**
   * The Files sidebar. Two layouts over the same folders:
   *
   * - `tree` — every vault as a root with folders and notes interleaved (the original view);
   * - `split` — folders on top, the selected folder's notes below, after Obsidian's File Tree
   *   Alternative. The bottom list either stops at the folder or reaches into its subfolders.
   *
   * Collapse state is shared by both, which is why it lives here rather than in `Tree`.
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
</script>

<div class="files" class:split={mode === 'split'} bind:this={host}>
  <div class="toolbar">
    <div class="modes">
      <button class:on={mode === 'tree'} onclick={() => setMode('tree')} title="Single tree" aria-label="Single tree">
        <Icon name="tree" />
      </button>
      <button class:on={mode === 'split'} onclick={() => setMode('split')} title="Folders and notes" aria-label="Folders and notes">
        <Icon name="split" />
      </button>
    </div>
    <span class="gap"></span>
    <button onclick={expandAll} title="Expand all" aria-label="Expand all"><Icon name="expand" /></button>
    <button onclick={collapseAll} title="Collapse all" aria-label="Collapse all"><Icon name="collapse" /></button>
    <button onclick={reveal} disabled={!activeId} title="Reveal the open note" aria-label="Reveal the open note">
      <Icon name="locate" />
    </button>
  </div>

  {#if mode === 'tree'}
    <Tree {vaults} {activeId} {activeVault} {collapsed} onToggle={toggle} {onOpen} {actions} />
  {:else}
    <div class="folders-wrap" style:height="{foldersHeight}px">
      <FolderTree {vaults} {selected} {collapsed} onToggle={toggle} onSelect={select} {actions} />
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
      {onOpen}
    />
  {/if}
</div>

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
