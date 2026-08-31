<script lang="ts">
  import { buildTree, countNotes, folderKey, type BrowserApi, type FolderNode, type TreeActions, type VaultNode } from '../lib/tree.ts'
  import Icon from './Icon.svelte'
  import { longpress } from '../lib/longpress.ts'

  /** Folders only, for the top half of the split view: one row per vault root and folder,
   *  clicking a name selects it and `NoteList` below shows what is in it. */
  let {
    vaults,
    selected,
    collapsed,
    onToggle,
    onSelect,
    browser,
    actions = {},
  }: {
    vaults: VaultNode[]
    selected: { vault: string; folder: string } | null
    collapsed: Record<string, boolean>
    onToggle: (key: string) => void
    onSelect: (vault: string, folder: string) => void
    browser: BrowserApi
    actions?: TreeActions
  } = $props()

  let trees = $derived(vaults.map((v) => ({ vault: v, root: buildTree(v.notes) })))

  function isSelected(vault: string, folder: string): boolean {
    return selected?.vault === vault && selected.folder === folder
  }
  const isDrop = (vault: string, folder: string) => browser.dropTarget?.vault === vault && browser.dropTarget.folder === folder

  /** Clicking the row you are already on folds it, so one click still gets you both. */
  function pick(vault: string, folder: string, key: string) {
    if (isSelected(vault, folder)) onToggle(key)
    else onSelect(vault, folder)
  }
</script>

{#snippet folders(vault: string, f: FolderNode, depth: number)}
  {#each f.folders as sub (sub.path)}
    {@const key = folderKey(vault, sub.path)}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="row folder"
      class:selected={isSelected(vault, sub.path)}
      class:drop={isDrop(vault, sub.path)}
      data-folder={key}
      style:padding-left="{depth * 0.9 + 0.4}rem"
      draggable="true"
      role="treeitem"
      aria-selected={isSelected(vault, sub.path)}
      tabindex="-1"
      ondragstart={(e) => browser.onFolderDragStart(vault, sub.path, e)}
      ondragend={browser.onDragEnd}
      ondragover={(e) => browser.onDragOver(vault, sub.path, e)}
      ondragleave={() => browser.onDragLeave(vault, sub.path)}
      ondrop={(e) => browser.onDrop(vault, sub.path, e)}
      oncontextmenu={(e) => browser.onFolderMenu(vault, sub.path, e)}
      use:longpress
    >
      {#if sub.folders.length}
        <button
          class="chev"
          class:open={!collapsed[key]}
          aria-label={collapsed[key] ? `Expand ${sub.name}` : `Collapse ${sub.name}`}
          onclick={() => onToggle(key)}>▸</button
        >
      {:else}
        <span class="chev spacer"></span>
      {/if}
      <button class="main" onclick={() => pick(vault, sub.path, key)} title={sub.path}>
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
      {@render folders(vault, sub, depth + 1)}
    {/if}
  {/each}
{/snippet}

<nav class="folders">
  {#each trees as t (t.vault.id)}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      class="row vault"
      class:selected={isSelected(t.vault.id, '')}
      class:drop={isDrop(t.vault.id, '')}
      data-folder={t.vault.id}
      role="treeitem"
      aria-selected={isSelected(t.vault.id, '')}
      tabindex="-1"
      ondragover={(e) => browser.onDragOver(t.vault.id, '', e)}
      ondragleave={() => browser.onDragLeave(t.vault.id, '')}
      ondrop={(e) => browser.onDrop(t.vault.id, '', e)}
      oncontextmenu={(e) => browser.onFolderMenu(t.vault.id, '', e)}
      use:longpress
    >
      <button
        class="chev"
        class:open={!collapsed[t.vault.id]}
        aria-label={collapsed[t.vault.id] ? `Expand ${t.vault.label}` : `Collapse ${t.vault.label}`}
        onclick={() => onToggle(t.vault.id)}>▸</button
      >
      <button class="main" onclick={() => pick(t.vault.id, '', t.vault.id)} title={t.vault.id}>
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
      {@render folders(t.vault.id, t.root, 1)}
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
  .folders {
    overflow: auto;
    font-size: 0.9rem;
    min-height: 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    width: 100%;
    padding: 0.2rem 0.4rem;
    border-radius: 4px;
  }
  .row:hover {
    background: var(--hover);
  }
  .row.selected {
    background: var(--accent-bg);
  }
  .row.drop {
    background: var(--accent-bg);
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .row.selected .name {
    color: var(--accent);
  }
  .main {
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
  .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .folder .name {
    font-weight: 600;
  }
  /* Same header treatment as the unified tree, so switching views does not move the eye. */
  .vault {
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
  .vault.selected .name,
  .vault.selected .main :global(svg) {
    color: var(--accent);
  }
  .chev {
    flex: none;
    width: 1em;
    border: 0;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--muted);
    cursor: pointer;
    transition: transform 0.1s;
  }
  .chev.open {
    transform: rotate(90deg);
  }
  .chev.spacer {
    cursor: default;
  }
  .count {
    color: var(--muted);
    font-size: 0.75em;
  }
  .actions {
    display: none;
    gap: 0.1rem;
  }
  .row:hover .actions {
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
  .add {
    border: 0;
    background: none;
    color: var(--muted);
    font: inherit;
    font-size: 0.85rem;
    text-align: left;
    cursor: pointer;
    margin-top: 0.2rem;
  }
  .empty {
    color: var(--muted);
    padding: 1rem;
    font-size: 0.85rem;
  }

  /* ---- touch: hover reveals nothing on a finger, and a row is a target rather than a pixel.
     Holding one opens its menu (lib/longpress.ts), so the platform must not select the text
     or raise its own callout underneath. */
  /* Both halves matter: some browsers report `hover: none` with a mouse attached (headless
     Chrome does), and a desktop must not grow a permanent row of buttons because of it. */
  @media (hover: none) and (pointer: coarse) {
    .actions {
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
    .actions button {
      font-size: 1em;
      padding: 0.3rem 0.45rem;
    }
  }
</style>
