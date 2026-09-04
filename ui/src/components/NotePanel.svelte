<script lang="ts" module>
  export type PanelTab = 'outline' | 'links' | 'history'
</script>

<script lang="ts">
  import { api, type NoteSummary } from '../lib/api.ts'
  import { displayName, type VaultSession } from '../lib/vault.svelte.ts'
  import OutlinePane, { type OutlineItem } from './OutlinePane.svelte'
  import HistoryPane from './HistoryPane.svelte'

  /**
   * Everything *about* the note you are reading, to the right of it: its outline, what links to
   * it, its tags, its versions.
   *
   * These used to be tabs in the left sidebar, which meant looking at the outline cost you the
   * file tree — two different questions ("where am I in this note" and "where is this note")
   * competing for one column. They are note-scoped, so they belong beside the note.
   */
  let {
    tab,
    session,
    noteId,
    headings,
    tags,
    onTab,
    onJump,
    onOpen,
    onAsk,
    onClose,
  }: {
    tab: PanelTab
    /** Absent until a note is open; the panel then shows why it is empty rather than nothing. */
    session: VaultSession | undefined
    noteId: string | null
    headings: OutlineItem[]
    tags: string[]
    onTab: (tab: PanelTab) => void
    onJump: (pos: number) => void
    onOpen: (id: string) => void
    onAsk: (title: string, initial: string) => Promise<string | null>
    onClose: () => void
  } = $props()

  const TABS: { id: PanelTab; label: string }[] = [
    { id: 'outline', label: 'Outline' },
    { id: 'links', label: 'Links' },
    { id: 'history', label: 'History' },
  ]

  let backlinks: NoteSummary[] = $state([])

  // Only fetched for the tab that shows them: backlinks are a round trip per note, and the
  // outline is the tab you are usually on.
  $effect(() => {
    const id = noteId
    const s = session
    if (!id || !s || tab !== 'links') {
      backlinks = []
      return
    }
    let live = true
    api
      .backlinks(s.id, id)
      .then((b) => live && (backlinks = b))
      .catch(() => live && (backlinks = []))
    return () => {
      live = false
    }
  })
</script>

<aside class="panel">
  <div class="tabs" role="tablist">
    {#each TABS as t (t.id)}
      <button class:on={tab === t.id} role="tab" aria-selected={tab === t.id} onclick={() => onTab(t.id)}>{t.label}</button>
    {/each}
    <button class="close" onclick={onClose} title="Hide this panel (Ctrl+Shift+R)" aria-label="Hide this panel">×</button>
  </div>

  {#if !session || !noteId}
    <p class="empty">Open a note to see its outline, links and history.</p>
  {:else if tab === 'outline'}
    <OutlinePane items={headings} {onJump} />
  {:else if tab === 'history'}
    <HistoryPane {session} {noteId} {onAsk} />
  {:else}
    <div class="body">
      <div class="section">
        <!-- The separator is an expression: Svelte trims the whitespace between text and a
             block, so a literal " · " here would come out flush against the word. -->
        <h2>Backlinks{#if backlinks.length}{` · ${backlinks.length}`}{/if}</h2>
        {#each backlinks as b (b.id)}
          <button class="backlink" onclick={() => onOpen(b.id)} title={b.path}>
            <span class="name">{b.title ?? displayName(b.path)}</span>
            <span class="ctx">{b.path}</span>
          </button>
        {/each}
        {#if backlinks.length === 0}<p class="none">Nothing links here yet.</p>{/if}
      </div>
      <div class="section">
        <h2>Tags</h2>
        <div class="tags">
          {#each tags as t (t)}<span class="tag">{t}</span>{/each}
          {#if tags.length === 0}<p class="none">Write <code>#a-tag</code> in the note to tag it.</p>{/if}
        </div>
      </div>
    </div>
  {/if}
</aside>

<style>
  .panel {
    display: grid;
    grid-template-rows: auto minmax(0, 1fr);
    min-height: 0;
    min-width: 0;
    background: var(--panel-2);
    border-left: 1px solid var(--border);
    overflow: hidden;
  }
  .tabs {
    display: flex;
    gap: 0.25rem;
    padding: 0.6rem 0.6rem 0.5rem;
  }
  .tabs button {
    flex: 1;
    font: inherit;
    font-size: 0.75rem;
    border: 1px solid transparent;
    background: none;
    color: var(--muted);
    padding: 0.25rem 0;
    border-radius: 6px;
    cursor: pointer;
  }
  .tabs button:hover:not(.on) {
    color: var(--fg);
    background: var(--hover);
  }
  /* The same raised chip the tab strip and the mode switch use. */
  .tabs button.on {
    color: var(--fg);
    font-weight: 600;
    background: var(--bg);
    border-color: var(--border);
  }
  .tabs .close {
    flex: none;
    width: 1.5rem;
    font-size: 0.95rem;
    line-height: 1;
    color: var(--faint);
  }
  .body {
    overflow: auto;
    min-height: 0;
  }
  .section {
    border-top: 1px solid var(--border-soft);
    padding: 0.75rem 0.875rem;
  }
  /* The panel is a stack of labelled shelves; these are the labels, and they should read as
     furniture rather than as content. */
  h2 {
    margin: 0 0 0.5rem;
    font-size: 0.65rem;
    font-weight: 700;
    letter-spacing: 0.09em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .backlink {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    width: 100%;
    text-align: left;
    font: inherit;
    border: 0;
    background: none;
    color: inherit;
    padding: 0.375rem 0.5rem;
    margin: 0 -0.5rem;
    border-radius: 6px;
    cursor: pointer;
  }
  .backlink:hover {
    background: var(--hover);
  }
  .backlink .name {
    font-size: 0.78rem;
    font-weight: 500;
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .backlink .ctx {
    font-size: 0.6875rem;
    color: var(--faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tags {
    display: flex;
    flex-wrap: wrap;
    gap: 0.375rem;
  }
  .tag {
    font-size: 0.72rem;
    color: var(--fg);
    background: var(--tag-bg);
    border-radius: 20px;
    padding: 0.1875rem 0.625rem;
  }
  .empty,
  .none {
    color: var(--faint);
    font-size: 0.75rem;
    line-height: 1.45;
    margin: 0;
  }
  .empty {
    padding: 0.75rem 0.875rem;
  }
  code {
    font-family: var(--mono);
    font-size: 0.95em;
  }

  @media (pointer: coarse) {
    .tabs button {
      padding: 0.45rem 0;
    }
    .backlink {
      padding-top: 0.5rem;
      padding-bottom: 0.5rem;
    }
  }
</style>
