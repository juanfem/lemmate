<script lang="ts">
  import Icon from './Icon.svelte'
  import ContextMenu, { menuAt, type MenuItem, type MenuState } from './ContextMenu.svelte'
  import { api, type FileEntry } from '../lib/api.ts'
  import type { VaultSession } from '../lib/vault.svelte.ts'
  import { buildFileTree, extension, fileKind, foldersToReveal, type FileKind, type FileNode } from '../lib/filetree.ts'
  import { longpress } from '../lib/longpress.ts'

  /**
   * The Files tab's attachments view (SPEC §9): every vault's files that are not notes, as a
   * tree of the folders that hold them. The notes' tree never shows a file; this never shows a
   * note. Opening a file puts it in a tab; everything else is on its row's menu.
   */
  let {
    vaults,
    activeVault,
    activeNoteId,
    activeFile,
    onOpenFile,
    onUpload,
    onRename,
    onDelete,
  }: {
    vaults: { id: string; label: string; session: VaultSession }[]
    /** The vault of whatever is open, for the toolbar's upload and locate. */
    activeVault: string | null
    activeNoteId: string | null
    /** The file in the focused tab, if one is: its row is marked. */
    activeFile: { vault: string; path: string } | null
    onOpenFile: (vault: string, path: string) => void
    /** Upload into `folder` (`''` the vault root), or wherever the dialog suggests (`null`). */
    onUpload: (vault: string, folder: string | null) => void
    onRename: (vault: string, entry: FileEntry) => void
    onDelete: (vault: string, entry: FileEntry) => void
  } = $props()

  $effect(() => {
    for (const v of vaults) v.session.watchFiles()
  })

  type Filter = 'all' | FileKind
  let filter: Filter = $state('all')
  const FILTERS: { id: Filter; label: string }[] = [
    { id: 'all', label: 'All' },
    { id: 'image', label: 'Images' },
    { id: 'style', label: 'Styles' },
    { id: 'other', label: 'Other' },
  ]
  let everything = $derived(vaults.flatMap((v) => v.session.files))
  let counts = $derived(
    Object.fromEntries(FILTERS.map((f) => [f.id, f.id === 'all' ? everything.length : everything.filter((e) => fileKind(e.path) === f.id).length])),
  )

  let trees = $derived(
    vaults.map((v) => ({
      vault: v,
      nodes: buildFileTree(v.session.files, (e) => filter === 'all' || fileKind(e.path) === filter),
    })),
  )

  // Open folders, by `<vault>:<path>`. Remembered: which decks you keep open is yours.
  const KEY = 'lemmate.filetree.open'
  function load(): Record<string, boolean> {
    try {
      return JSON.parse(localStorage.getItem(KEY) ?? '{}') as Record<string, boolean>
    } catch {
      return {}
    }
  }
  let open: Record<string, boolean> = $state(load())
  function persist() {
    try {
      localStorage.setItem(KEY, JSON.stringify(open))
    } catch {
      /* the view just forgets */
    }
  }
  const folderKey = (vault: string, path: string) => `${vault}:${path}`
  function toggle(vault: string, path: string) {
    const k = folderKey(vault, path)
    open = { ...open, [k]: !open[k] }
    persist()
  }
  /** The vault headers start open; folders start closed. */
  const isOpen = (vault: string, path: string) => open[folderKey(vault, path)] ?? path === ''
  /** The toolbar's buttons live in the Files toolbar (`FilesPane`), which calls these. */
  export function collapseAll() {
    open = Object.fromEntries(vaults.map((v) => [folderKey(v.id, ''), true]))
    persist()
  }

  /** Open the folders holding the files the open note uses, and scroll the first into view. */
  let host: HTMLElement | undefined = $state()
  export function locate() {
    const v = vaults.find((x) => x.id === activeVault)
    if (!v || !activeNoteId) return
    const used = v.session.files.filter((f) => f.used_by.includes(activeNoteId!)).map((f) => f.path)
    if (!used.length) return
    const next = { ...open, [folderKey(v.id, '')]: true }
    for (const dir of foldersToReveal(used)) next[folderKey(v.id, dir)] = true
    open = next
    persist()
    filter = 'all'
    requestAnimationFrame(() => host?.querySelector(`[data-file="${CSS.escape(folderKey(v.id, used[0]!))}"]`)?.scrollIntoView({ block: 'nearest' }))
  }
  export function canLocate(): boolean {
    return locatable
  }
  // The section on top: the files the open note uses, the same list as the Files shelf at the
  // foot of the note. Opening one focuses its file tab, which leaves no open note, so the list
  // stays on the note it was showing until another note (or a blank tab) takes the focus.
  let shown: { vault: string; note: string } | null = $state(null)
  $effect(() => {
    if (activeNoteId && activeVault) shown = { vault: activeVault, note: activeNoteId }
    else if (!activeFile) shown = null
  })
  let inNote = $derived.by(() => {
    const v = shown && vaults.find((x) => x.id === shown!.vault)
    if (!v || !shown) return null
    const path = v.session.pathOf(shown.note)
    if (path === undefined) return null
    const note = shown.note
    return {
      vault: v.id,
      name: path.slice(path.lastIndexOf('/') + 1).replace(/\.md$/, ''),
      entries: v.session.files.filter((f) => f.used_by.includes(note)),
    }
  })
  let locatable = $derived(
    !!activeNoteId && !!vaults.find((x) => x.id === activeVault)?.session.files.some((f) => f.used_by.includes(activeNoteId!)),
  )
  // Folded or not, remembered like the folders are.
  const NOTE_KEY = 'lemmate.filetree.note'
  let noteOpen = $state(read(NOTE_KEY) !== '0')
  function read(key: string): string | null {
    try {
      return localStorage.getItem(key)
    } catch {
      return null
    }
  }
  function toggleNote() {
    noteOpen = !noteOpen
    try {
      localStorage.setItem(NOTE_KEY, noteOpen ? '1' : '0')
    } catch {
      /* the view just forgets */
    }
  }

  let menu: MenuState | null = $state(null)
  function fileMenu(vault: string, entry: FileEntry, e: MouseEvent) {
    const items: MenuItem[] = [
      { label: 'Open', run: () => onOpenFile(vault, entry.path) },
      { label: 'Download', run: () => download(vault, entry) },
      { label: 'Rename / move…', run: () => onRename(vault, entry) },
      { label: '', separator: true },
      { label: 'Delete…', danger: true, run: () => onDelete(vault, entry) },
    ]
    menu = menuAt(e, items)
  }
  function folderMenu(vault: string, path: string, e: MouseEvent) {
    menu = menuAt(e, [{ label: path ? 'Upload files here…' : 'Upload files to the vault root…', run: () => onUpload(vault, path) }])
  }
  function download(vault: string, entry: FileEntry) {
    const a = document.createElement('a')
    a.href = api.attachmentUrl(vault, entry.hash)
    a.download = entry.path.slice(entry.path.lastIndexOf('/') + 1)
    a.click()
  }

  /** What a row says after its name: who uses the file, or why it is here though nobody does. */
  function usage(e: FileEntry): string {
    if (e.vault) return 'vault'
    if (e.used_by.length) return e.used_by.length === 1 ? '1 note' : `${e.used_by.length} notes`
    return e.kept ? 'kept' : 'unused'
  }
  const BADGE: Record<string, string> = { style: 'var(--syn-keyword)', other: 'var(--muted)' }
</script>

{#snippet file(vault: string, entry: FileEntry, name: string, indent: string, tree = true)}
  <button
    class="row file"
    class:active={activeFile?.vault === vault && activeFile.path === entry.path}
    data-file={tree ? folderKey(vault, entry.path) : undefined}
    style:padding-left={indent}
    onclick={() => onOpenFile(vault, entry.path)}
    oncontextmenu={(e) => fileMenu(vault, entry, e)}
    use:longpress
    title={entry.path}
  >
    {#if fileKind(entry.path) === 'image'}
      <img class="thumb" src={api.attachmentUrl(vault, entry.hash)} alt="" loading="lazy" />
    {:else}
      <span class="badge" style:color={BADGE[fileKind(entry.path)]}>{extension(entry.path).slice(0, 4).toUpperCase() || 'FILE'}</span>
    {/if}
    <span class="name">{name}</span>
    <span class="count" class:quiet={!entry.used_by.length && !entry.vault}>{usage(entry)}</span>
  </button>
{/snippet}

{#snippet rows(vault: string, nodes: FileNode[], depth: number)}
  {#each nodes as n (n.path)}
    {#if n.kind === 'folder'}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="row folder" style:padding-left="{depth * 0.9 + 0.4}rem" oncontextmenu={(e) => folderMenu(vault, n.path, e)} use:longpress>
        <button class="main" onclick={() => toggle(vault, n.path)} aria-expanded={isOpen(vault, n.path)} title={n.path}>
          <span class="chev" class:open={isOpen(vault, n.path)}>▸</span>
          <span class="name">{n.label}</span>
          <span class="count">{n.count}</span>
        </button>
        <button class="here" onclick={() => onUpload(vault, n.path)} title="Upload files here" aria-label="Upload files into {n.label}">
          <Icon name="upload" size={12} />
        </button>
      </div>
      {#if isOpen(vault, n.path)}
        {@render rows(vault, n.children, depth + 1)}
      {/if}
    {:else}
      {@render file(vault, n.entry, n.name, `${depth * 0.9 + 1.3}rem`)}
    {/if}
  {/each}
{/snippet}

<div class="attachments" bind:this={host}>
  {#if inNote}
    <section class="note" aria-label="Files this note uses">
      <button class="row head" onclick={toggleNote} aria-expanded={noteOpen}>
        <span class="chev" class:open={noteOpen}>▸</span>
        <span class="name" title="The files {inNote.name} uses">{inNote.name}</span>
        <span class="count">{inNote.entries.length}</span>
      </button>
      {#if noteOpen}
        {#each inNote.entries as e (e.path)}
          {@render file(inNote.vault, e, e.path.slice(e.path.lastIndexOf('/') + 1), '1.3rem', false)}
        {:else}
          <p class="empty small">This note uses no files.</p>
        {/each}
      {/if}
    </section>
  {/if}
  <div class="bar">
    <div class="chips" role="group" aria-label="Show">
      {#each FILTERS as f (f.id)}
        {#if f.id === 'all' || counts[f.id] || filter === f.id}
          <button class:on={filter === f.id} onclick={() => (filter = f.id)} aria-pressed={filter === f.id}>{f.label} {counts[f.id]}</button>
        {/if}
      {/each}
    </div>
  </div>
  <nav class="tree" aria-label="Attachments">
    {#each trees as t (t.vault.id)}
      {#if vaults.length > 1}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div class="row vault" oncontextmenu={(e) => folderMenu(t.vault.id, '', e)} use:longpress>
          <button class="main" onclick={() => toggle(t.vault.id, '')} aria-expanded={isOpen(t.vault.id, '')}>
            <span class="chev" class:open={isOpen(t.vault.id, '')}>▸</span>
            <span class="name">{t.vault.label}</span>
            <span class="count">{t.vault.session.files.length}</span>
          </button>
          <button class="here" onclick={() => onUpload(t.vault.id, '')} title="Upload files to this vault's root" aria-label="Upload files to {t.vault.label}"><Icon name="upload" size={12} /></button>
        </div>
      {/if}
      {#if vaults.length === 1 || isOpen(t.vault.id, '')}
        {@render rows(t.vault.id, t.nodes, vaults.length > 1 ? 1 : 0)}
        {#if t.nodes.length === 0}
          <p class="empty" class:small={vaults.length > 1}>
            {filter === 'all' ? 'No files yet. Upload one, or paste an image into a note.' : 'None of these here.'}
          </p>
        {/if}
      {/if}
    {/each}
  </nav>
</div>

{#if menu}
  <ContextMenu {menu} onClose={() => (menu = null)} />
{/if}

<style>
  .attachments {
    display: flex;
    flex-direction: column;
    min-height: 0;
    flex: 1;
  }
  .note {
    padding: 0 0 0.4rem;
    margin-bottom: 0.4rem;
    border-bottom: 1px solid var(--border);
    font-size: 0.9rem;
    max-height: 40%;
    overflow: auto;
    flex-shrink: 0;
  }
  .note .head .name {
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--muted);
  }
  .note .head .name::before {
    content: 'Files in ';
    font-weight: 400;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 0.15rem;
    padding: 0 0.5rem 0.35rem;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
  }
  .chips button {
    border: 0;
    border-radius: 999px;
    padding: 0.1rem 0.5rem;
    font: inherit;
    font-size: 0.72rem;
    background: var(--tag-bg);
    color: var(--fg);
    cursor: pointer;
  }
  .chips button.on {
    background: var(--fg);
    color: var(--bg);
  }
  .tree {
    overflow: auto;
    font-size: 0.9rem;
    padding-bottom: 0.5rem;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    width: 100%;
    border: 0;
    background: none;
    color: inherit;
    text-align: left;
    padding: 0.2rem 0.4rem;
    border-radius: 4px;
    font: inherit;
    cursor: pointer;
  }
  .row:hover {
    background: var(--hover);
  }
  .row.active {
    background: var(--sel-bg);
    color: var(--sel-fg);
  }
  .main {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.3rem;
    min-width: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    padding: 0;
    cursor: pointer;
  }
  .folder .name {
    font-weight: 600;
  }
  .vault {
    border-top: 1px solid var(--border);
    background: color-mix(in srgb, var(--border) 30%, transparent);
    border-radius: 0;
  }
  .vault:first-child {
    border-top: 0;
  }
  .vault .name {
    font-size: 0.72rem;
    font-weight: 700;
    text-transform: uppercase;
    letter-spacing: 0.07em;
    color: var(--muted);
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .chev {
    display: inline-block;
    width: 0.8em;
    transition: transform 0.1s;
    color: var(--muted);
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .count {
    color: var(--muted);
    font-size: 0.75em;
    white-space: nowrap;
  }
  .count.quiet {
    font-style: italic;
  }
  .row.active .count {
    color: var(--sel-muted);
  }
  /* Where a folder's count is, until the pointer is on the row: then the button that puts a
     file in it. The count is on the button's other side, so nothing jumps. */
  .here {
    display: none;
    align-items: center;
    justify-content: center;
    width: 1.35rem;
    height: 1.2rem;
    border: 1px solid var(--border);
    border-radius: 4px;
    background: var(--bg);
    color: var(--accent);
    cursor: pointer;
    padding: 0;
  }
  .folder:hover .here,
  .vault:hover .here {
    display: inline-flex;
  }
  .thumb {
    width: 1.6rem;
    height: 1.1rem;
    object-fit: cover;
    border-radius: 2px;
    background: var(--code-bg);
    flex-shrink: 0;
  }
  .badge {
    width: 1.6rem;
    flex-shrink: 0;
    font-size: 0.55rem;
    font-weight: 700;
    letter-spacing: 0.02em;
  }
  .empty {
    color: var(--muted);
    padding: 0.8rem;
    font-size: 0.85rem;
    margin: 0;
  }
  .empty.small {
    padding: 0.2rem 0.4rem 0.4rem 1.7rem;
    font-size: 0.8rem;
  }
  @media (hover: none) and (pointer: coarse) {
    .here {
      display: inline-flex;
    }
  }
  @media (pointer: coarse) {
    .row {
      padding-top: 0.5rem;
      padding-bottom: 0.5rem;
      user-select: none;
      -webkit-user-select: none;
      -webkit-touch-callout: none;
    }
  }
</style>
