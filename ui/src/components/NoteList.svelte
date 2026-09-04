<script lang="ts">
  import { displayName, type NoteEntry } from '../lib/vault.svelte.ts'
  import { folderOf, type BrowserApi } from '../lib/tree.ts'
  import { longpress } from '../lib/longpress.ts'

  /** The bottom half of the split view: a flat list of the selected folder's notes. With
   *  subfolders included, each row carries the folder it came from so the list stays readable. */
  let {
    notes,
    base,
    activeId,
    dates = {},
    showFolders = false,
    empty = 'No notes here.',
    browser,
  }: {
    notes: NoteEntry[]
    /** Selected folder path, stripped from the sub-folder hint on each row. */
    base: string
    activeId: string | null
    /** note id → ISO timestamp of its last change, where the listing knew one. */
    dates?: Record<string, string>
    showFolders?: boolean
    empty?: string
    browser: BrowserApi
  } = $props()

  /**
   * How long ago, at about the precision you care about at that distance: minutes for the last
   * hour, a clock time for today, a weekday for this week, then a date. A vault this size is
   * unscannable as a column of bare filenames, and "when did I last touch it" is the question
   * the list is usually being asked.
   */
  function when(iso: string | undefined): string {
    if (!iso) return ''
    const d = new Date(iso)
    if (Number.isNaN(d.getTime())) return ''
    const now = new Date()
    const ms = now.getTime() - d.getTime()
    if (ms < 60_000) return 'now'
    if (ms < 3_600_000) return `${Math.floor(ms / 60_000)}m`
    if (d.toDateString() === now.toDateString()) return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
    if (ms < 6 * 86_400_000) return d.toLocaleDateString(undefined, { weekday: 'short' })
    if (d.getFullYear() === now.getFullYear()) return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
    return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short' })
  }

  function hint(path: string): string {
    const folder = folderOf(path)
    if (!folder) return ''
    if (!base) return folder
    return folder === base ? '' : folder.slice(base.length + 1)
  }
</script>

<nav class="notes">
  {#each notes as n (n.id)}
    <button
      class="row"
      class:active={n.id === activeId}
      class:selected={browser.selected.has(n.id)}
      data-note={n.id}
      draggable="true"
      onclick={(e) => browser.onNoteClick(n.id, e)}
      oncontextmenu={(e) => browser.onNoteMenu(n.id, e)}
      ondragstart={(e) => browser.onNoteDragStart(n.id, e)}
      ondragend={browser.onDragEnd}
      use:longpress
      title={n.path}
    >
      <span class="name">{displayName(n.path)}</span>
      {#if showFolders && hint(n.path)}<span class="hint">{hint(n.path)}</span>{/if}
      {#if !showFolders || !hint(n.path)}<span class="spacer"></span>{/if}
      <span class="when">{when(dates[n.id])}</span>
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
  .row.selected {
    background: var(--accent-bg);
  }
  .row.active {
    background: var(--accent-bg);
    color: var(--sel-fg);
    font-weight: 600;
  }
  .row.active .when {
    color: var(--sel-muted);
  }
  .row.active.selected {
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .spacer {
    flex: 1;
  }
  .when {
    flex: none;
    padding-left: 0.5rem;
    color: var(--faint);
    font-size: 0.72em;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .hint {
    margin-left: auto;
    flex: none;
    color: var(--faint);
    font-size: 0.72em;
    max-width: 40%;
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

  /* Holding a row opens its menu (lib/longpress.ts): no text selection, no platform callout. */
  @media (pointer: coarse) {
    .row {
      padding-top: 0.55rem;
      padding-bottom: 0.55rem;
      user-select: none;
      -webkit-user-select: none;
      -webkit-touch-callout: none;
    }
  }
</style>
