<script lang="ts">
  import { displayName, type NoteEntry } from '../lib/vault.svelte.ts'
  import { folderOf } from '../lib/tree.ts'

  /** The bottom half of the split view: a flat list of the selected folder's notes. With
   *  subfolders included, each row carries the folder it came from so the list stays readable. */
  let {
    notes,
    base,
    activeId,
    showFolders = false,
    empty = 'No notes here.',
    onOpen,
  }: {
    notes: NoteEntry[]
    /** Selected folder path, stripped from the sub-folder hint on each row. */
    base: string
    activeId: string | null
    showFolders?: boolean
    empty?: string
    onOpen: (id: string) => void
  } = $props()

  function hint(path: string): string {
    const folder = folderOf(path)
    if (!folder) return ''
    if (!base) return folder
    return folder === base ? '' : folder.slice(base.length + 1)
  }
</script>

<nav class="notes">
  {#each notes as n (n.id)}
    <button class="row" class:active={n.id === activeId} data-note={n.id} onclick={() => onOpen(n.id)} title={n.path}>
      <span class="name">{displayName(n.path)}</span>
      {#if showFolders && hint(n.path)}<span class="hint">{hint(n.path)}</span>{/if}
    </button>
  {/each}
  {#if notes.length === 0}
    <p class="empty">{empty}</p>
  {/if}
</nav>

<style>
  .notes {
    overflow: auto;
    min-height: 0;
    font-size: 0.9rem;
    padding-bottom: 0.3rem;
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: 0.4rem;
    width: 100%;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    text-align: left;
    padding: 0.25rem 0.6rem;
    border-radius: 4px;
    cursor: pointer;
  }
  .row:hover {
    background: var(--hover);
  }
  .row.active {
    background: var(--accent-bg);
    color: var(--accent);
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hint {
    margin-left: auto;
    flex: none;
    color: var(--muted);
    font-size: 0.72em;
    max-width: 45%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
  }
  .empty {
    color: var(--muted);
    padding: 0.8rem 0.6rem;
    font-size: 0.85rem;
  }
</style>
