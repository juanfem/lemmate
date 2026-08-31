<script lang="ts" module>
  import type { ViewMode } from '../lib/editor/setup.ts'

  /** One editing pane: its own tab strip, note header, editor and backlinks (SPEC §9). */
  export interface PaneState {
    id: number
    tabs: string[]
    active: string | null
    /** SPEC §8 view mode, per pane — a source pane can sit beside a reading one. */
    mode: ViewMode
  }

  /**
   * An empty tab, opened by the ＋ on the strip and waiting for a note. Held as a tab id so
   * the rest of the pane machinery — ordering, closing, activating, persisting — needs no
   * special case; only the label and the "is there a note here" check look at it.
   */
  export function isBlank(tabId: string): boolean {
    return tabId.startsWith('blank:')
  }
</script>

<script lang="ts">
  import { api, type NoteSummary } from '../lib/api.ts'
  import { displayName, type VaultSession } from '../lib/vault.svelte.ts'
  import Editor from './Editor.svelte'
  import { VIEW_MODES } from '../lib/editor/setup.ts'
  import Icon from './Icon.svelte'
  import type { OutlineItem } from './OutlinePane.svelte'

  let {
    lookup,
    vaultLabel,
    pane,
    focused,
    pinned = [],
    onActivate,
    onClose,
    onFocus,
    onBookmark,
    onShare,
    onRename,
    onDelete,
    onOpen,
    onHeadings,
    onPresence,
    onMode,
    onNewTab,
    jumpTo = $bindable(),
  }: {
    /** Which vault a tab belongs to; tabs in one pane may come from different vaults. */
    lookup: (noteId: string) => VaultSession | undefined
    /** Vault label for the note header, empty when there is only one vault to speak of. */
    vaultLabel?: (noteId: string) => string
    pane: PaneState
    focused: boolean
    pinned?: string[]
    onActivate: (id: string) => void
    onClose: (id: string) => void
    onFocus: () => void
    onBookmark: () => void
    onShare: () => void
    onRename: () => void
    onDelete: () => void
    onOpen: (id: string) => void
    onHeadings?: (items: OutlineItem[]) => void
    onPresence?: (names: string[]) => void
    onMode?: (mode: ViewMode) => void
    onNewTab?: () => void
    jumpTo?: (pos: number) => void
  } = $props()

  let host: HTMLElement
  let presence: string[] = $state([])
  let backlinks: NoteSummary[] = $state([])

  /** Reactive path lookup: `session.notes` updates on rename, `pathOf` alone would not. */
  function pathOf(id: string): string | undefined {
    const s = lookup(id)
    return s?.notes.find((n) => n.id === id)?.path ?? s?.pathOf(id)
  }
  let session = $derived(pane.active ? lookup(pane.active) : undefined)
  let activePath = $derived(
    pane.active ? (pathOf(pane.active) ?? (session?.noteOnly ? 'shared note' : '(deleted)')) : '',
  )
  // Pinned tabs sort first; the rest keep the order they were opened in.
  let tabs = $derived([...pane.tabs].sort((a, b) => Number(pinned.includes(b)) - Number(pinned.includes(a))))

  $effect(() => {
    const id = pane.active
    const s = session
    if (!id || !s) {
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

  // A declarative onmousedown would trip svelte a11y on a non-interactive element; this
  // catches clicks anywhere in the pane (the editor included) without a role.
  $effect(() => {
    const node = host
    const focus = () => onFocus()
    node.addEventListener('mousedown', focus)
    return () => node.removeEventListener('mousedown', focus)
  })
</script>

<section class="pane" class:focused bind:this={host} onfocusin={onFocus}>
  <div class="tabs">
    {#each tabs as id (id)}
      <button class="tab" class:active={id === pane.active} class:blank={isBlank(id)} onclick={() => onActivate(id)} title={pathOf(id)}>
        {#if pinned.includes(id)}<span class="pin" title="Pinned">•</span>{/if}
        {isBlank(id) ? 'New tab' : displayName(pathOf(id) ?? id)}
        {#if !pinned.includes(id)}
          <span class="x" role="button" tabindex="-1" onclick={(e) => { e.stopPropagation(); onClose(id) }} onkeydown={() => {}}>×</span>
        {/if}
      </button>
    {/each}
    {#if onNewTab}
      <button class="newtab" onclick={onNewTab} title="New tab (Ctrl+T)" aria-label="New tab"><Icon name="plus" size={13} /></button>
    {/if}
  </div>
  {#if pane.active && session && !isBlank(pane.active)}
    {#key pane.active}
      <div class="note-head">
        {#if vaultLabel?.(pane.active)}<span class="vault">{vaultLabel(pane.active)}</span>{/if}
        <span class="path">{activePath}</span>
        {#if presence.length}
          <span class="presence" title={presence.join(', ')}>· with {presence.length === 1 ? presence[0] : `${presence.length} others`}</span>
        {/if}
        <span class="spacer"></span>
        <span class="modes" role="group" aria-label="View mode">
          {#each VIEW_MODES as m (m.id)}
            <button class:on={pane.mode === m.id} onclick={() => onMode?.(m.id)} title={m.hint} aria-pressed={pane.mode === m.id}>{m.label}</button>
          {/each}
        </span>
        <button onclick={onBookmark} title="Bookmark (Ctrl+Shift+B)">{session.isBookmarked('note', activePath) ? '★' : '☆'}</button>
        {#if !session.noteOnly}<button onclick={onShare} title="Share">Share</button>{/if}
        <button onclick={onRename} title="Rename / move">Rename</button>
        <button onclick={onDelete} title="Move to trash">Delete</button>
      </div>
      <div class="editor-wrap">
        <Editor
          {session}
          noteId={pane.active}
          {onOpen}
          onHeadings={(h) => onHeadings?.(h)}
          onPresence={(p) => { presence = p; onPresence?.(p) }}
          mode={pane.mode}
          bind:jumpTo
        />
      </div>
      {#if backlinks.length}
        <div class="backlinks">
          <strong>Linked from</strong>
          {#each backlinks as b (b.id)}
            <button onclick={() => onOpen(b.id)}>{b.title ?? displayName(b.path)}</button>
          {/each}
        </div>
      {/if}
    {/key}
  {:else}
    <div class="placeholder">
      <p>Open a note from the tree, or press <kbd>Ctrl</kbd>+<kbd>O</kbd>.</p>
    </div>
  {/if}
</section>

<style>
  .pane {
    display: grid;
    grid-template-rows: auto auto 1fr auto;
    min-width: 0;
    min-height: 0;
    flex: 1 1 0;
    border-top: 2px solid transparent;
  }
  .pane.focused {
    border-top-color: var(--accent);
  }
  .tabs {
    display: flex;
    overflow-x: auto;
    border-bottom: 1px solid var(--border);
    background: var(--panel);
  }
  .tab {
    font: inherit;
    font-size: 0.85rem;
    border: 0;
    border-right: 1px solid var(--border);
    background: none;
    color: var(--muted);
    padding: 0.4rem 0.8rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .tab.active {
    color: var(--fg);
    background: var(--bg);
  }
  .tab.blank {
    font-style: italic;
  }
  /* Sits after the last tab rather than pinned to the right, the way a browser strip does. */
  .newtab {
    display: grid;
    place-items: center;
    border: 0;
    background: none;
    color: var(--muted);
    padding: 0.4rem 0.7rem;
    cursor: pointer;
    flex: none;
  }
  .newtab:hover {
    color: var(--fg);
    background: var(--hover);
  }
  .tab .x {
    margin-left: 0.5rem;
    opacity: 0.6;
  }
  .tab .pin {
    color: var(--accent);
    margin-right: 0.25rem;
  }
  .note-head .vault {
    color: var(--muted);
    text-transform: uppercase;
    font-size: 0.75em;
    letter-spacing: 0.03em;
  }
  .note-head .vault::after {
    content: ' /';
  }
  .note-head {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.2rem 1rem;
    font-size: 0.8rem;
    color: var(--muted);
    border-bottom: 1px solid var(--border);
  }
  .note-head button,
  .backlinks button {
    font: inherit;
    font-size: 0.85rem;
    border: 0;
    background: none;
    color: var(--muted);
    padding: 0.2rem 0.5rem;
    border-radius: 4px;
    cursor: pointer;
  }
  .spacer {
    flex: 1;
  }
  /* One segmented control, so the three modes read as a choice rather than three buttons. */
  .modes {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 5px;
    overflow: hidden;
    margin-right: 0.3rem;
  }
  .modes button {
    border-radius: 0;
    padding: 0.1rem 0.45rem;
    font-size: 0.75rem;
  }
  .modes button + button {
    border-left: 1px solid var(--border);
  }
  .modes button.on {
    background: var(--accent-bg);
    color: var(--accent);
  }
  .presence {
    color: var(--accent);
  }
  .editor-wrap {
    min-height: 0;
  }
  .backlinks {
    border-top: 1px solid var(--border);
    padding: 0.4rem 1rem;
    font-size: 0.85rem;
    display: flex;
    gap: 0.4rem;
    flex-wrap: wrap;
    align-items: center;
  }
  .backlinks button {
    color: var(--accent);
  }
  .placeholder {
    display: grid;
    place-items: center;
    color: var(--muted);
  }
</style>
