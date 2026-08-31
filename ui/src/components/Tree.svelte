<script lang="ts">
  import { displayName } from '../lib/vault.svelte.ts'
  import { buildTree, countNotes, folderKey, type BrowserApi, type FolderNode, type TreeActions, type VaultNode } from '../lib/tree.ts'
  import Icon from './Icon.svelte'

  /**
   * The unified view: every vault as a root, folders and notes interleaved beneath it.
   * Collapse state and selection live in `FilesPane` — expand-all, reveal, shift-click ranges
   * and drag-and-drop all reach across both views, so neither view may own them.
   */
  let {
    vaults,
    activeId,
    activeVault,
    collapsed,
    onToggle,
    browser,
    actions = {},
  }: {
    vaults: VaultNode[]
    activeId: string | null
    activeVault?: string | null
    collapsed: Record<string, boolean>
    onToggle: (key: string) => void
    browser: BrowserApi
    actions?: TreeActions
  } = $props()

  let trees = $derived(vaults.map((v) => ({ vault: v, root: buildTree(v.notes) })))
  const isDrop = (vault: string, folder: string) => browser.dropTarget?.vault === vault && browser.dropTarget.folder === folder
</script>

{#snippet folder(vault: string, f: FolderNode, depth: number)}
  {#each f.folders as sub (sub.path)}
    {@const key = folderKey(vault, sub.path)}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="row folder"
      class:drop={isDrop(vault, sub.path)}
      style:padding-left="{depth * 0.9 + 0.4}rem"
      draggable="true"
      role="treeitem"
      aria-selected="false"
      tabindex="-1"
      ondragstart={(e) => browser.onFolderDragStart(vault, sub.path, e)}
      ondragend={browser.onDragEnd}
      ondragover={(e) => browser.onDragOver(vault, sub.path, e)}
      ondragleave={() => browser.onDragLeave(vault, sub.path)}
      ondrop={(e) => browser.onDrop(vault, sub.path, e)}
      oncontextmenu={(e) => browser.onFolderMenu(vault, sub.path, e)}
    >
      <button class="folder-main" onclick={() => onToggle(key)}>
        <span class="chev" class:open={!collapsed[key]}>▸</span>
        <span class="name">{sub.name}</span>
        <span class="count">{countNotes(sub)}</span>
      </button>
      <span class="actions">
        {#if actions.onCreateIn}<button title="New note here" onclick={() => actions.onCreateIn?.(vault, sub.path)}>＋</button>{/if}
        {#if actions.onRenameFolder}<button title="Rename / move folder" onclick={() => actions.onRenameFolder?.(vault, sub.path)}>✎</button>{/if}
        {#if actions.onDeleteFolder}<button title="Move folder to trash" onclick={() => actions.onDeleteFolder?.(vault, sub.path)}>🗑</button>{/if}
      </span>
    </div>
    {#if !collapsed[key]}
      {@render folder(vault, sub, depth + 1)}
    {/if}
  {/each}
  {#each f.notes as n (n.id)}
    <button
      class="row note"
      class:active={n.id === activeId}
      class:selected={browser.selected.has(n.id)}
      data-note={n.id}
      draggable="true"
      style:padding-left="{depth * 0.9 + 1.3}rem"
      onclick={(e) => browser.onNoteClick(n.id, e)}
      oncontextmenu={(e) => browser.onNoteMenu(n.id, e)}
      ondragstart={(e) => browser.onNoteDragStart(n.id, e)}
      ondragend={browser.onDragEnd}
      title={n.path}
    >
      <span class="name">{displayName(n.path)}</span>
    </button>
  {/each}
{/snippet}

<nav class="tree">
  {#each trees as t (t.vault.id)}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="row vault"
      class:current={t.vault.id === activeVault}
      class:drop={isDrop(t.vault.id, '')}
      role="treeitem"
      aria-selected="false"
      tabindex="-1"
      ondragover={(e) => browser.onDragOver(t.vault.id, '', e)}
      ondragleave={() => browser.onDragLeave(t.vault.id, '')}
      ondrop={(e) => browser.onDrop(t.vault.id, '', e)}
      oncontextmenu={(e) => browser.onFolderMenu(t.vault.id, '', e)}
    >
      <button class="folder-main" onclick={() => onToggle(t.vault.id)} title={t.vault.id}>
        <span class="chev" class:open={!collapsed[t.vault.id]}>▸</span>
        <Icon name="vault" size={12} />
        <span class="name">{t.vault.label}</span>
        <span class="count">{t.vault.notes.length}</span>
      </button>
      <span class="actions">
        {#if actions.onCreateInVault}<button title="New note in this vault" onclick={() => actions.onCreateInVault?.(t.vault.id)}>＋</button>{/if}
        {#if actions.onRenameVault}<button title="Rename vault" onclick={() => actions.onRenameVault?.(t.vault.id)}>✎</button>{/if}
        {#if actions.onImportInto}<button title="Import an Obsidian vault here" onclick={() => actions.onImportInto?.(t.vault.id)}>⇥</button>{/if}
      </span>
    </div>
    {#if !collapsed[t.vault.id]}
      {@render folder(t.vault.id, t.root, 1)}
      {#if t.vault.notes.length === 0}
        <p class="empty small">Empty vault.</p>
      {/if}
    {/if}
  {/each}
  {#if actions.onNewVault}
    <button class="row add" onclick={actions.onNewVault}>＋ New vault</button>
  {/if}
  {#if vaults.length === 0}
    <p class="empty">No vaults yet.</p>
  {/if}
</nav>

<style>
  .tree {
    overflow: auto;
    font-size: 0.9rem;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    width: 100%;
    border: 0;
    background: none;
    color: inherit;
    text-align: left;
    padding: 0.2rem 0.4rem;
    cursor: pointer;
    border-radius: 4px;
    font: inherit;
  }
  .row:hover {
    background: var(--hover);
  }
  /* Selection is the weaker mark, the open note the stronger one: a note can be both. */
  .row.selected {
    background: var(--accent-bg);
  }
  .row.active {
    background: var(--accent-bg);
    color: var(--accent);
  }
  .row.active.selected {
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .row.drop {
    background: var(--accent-bg);
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .folder .name {
    font-weight: 600;
  }
  /* A vault heads its own subtree, so it reads as a section header rather than a folder:
     small caps, wide tracking, its own band — never mistakable for a `Daily/` below it. */
  .vault {
    padding-right: 0.2rem;
    border-top: 1px solid var(--border);
    background: color-mix(in srgb, var(--border) 30%, transparent);
    border-radius: 0;
    margin-bottom: 0.15rem;
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
  .vault.current .name,
  .vault.current .folder-main :global(svg) {
    color: var(--accent);
  }
  .folder {
    padding-right: 0.2rem;
  }
  .folder-main {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 0.3rem;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    padding: 0;
    cursor: pointer;
    min-width: 0;
  }
  .actions {
    display: none;
    gap: 0.1rem;
  }
  .folder:hover .actions,
  .vault:hover .actions {
    display: inline-flex;
  }
  .actions button {
    border: 0;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: 0.8em;
    cursor: pointer;
    padding: 0 0.2rem;
  }
  .actions button:hover {
    color: var(--fg);
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
  }
  .add {
    color: var(--muted);
    font-size: 0.85rem;
    margin-top: 0.2rem;
  }
  .empty {
    color: var(--muted);
    padding: 1rem;
  }
  .empty.small {
    padding: 0.2rem 0.4rem 0.4rem 1.7rem;
    font-size: 0.8rem;
  }
</style>
