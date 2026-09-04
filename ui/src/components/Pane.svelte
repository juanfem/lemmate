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
  import ContextMenu, { menuAt, type MenuState } from './ContextMenu.svelte'
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
    /** Absent where sharing has nowhere to happen — a vault held by a local relay. */
    onShare?: () => void
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

  /** The path as a trail — folders, then the note. The note is what you are looking at, so it
   *  is the only part drawn at full strength; the folders are context. */
  let crumbs = $derived.by(() => {
    const parts = activePath.split('/').filter(Boolean)
    const name = parts.pop() ?? ''
    return { folders: parts, name: displayName(name) }
  })
  /** Quarto notes are still markdown, but saying so is the point of the label. */
  let format = $derived(activePath.endsWith('.qmd') ? 'Quarto' : 'Markdown')
  let stats: { lines: number; words: number } = $state({ lines: 0, words: 0 })

  // Rename, share and delete all live in `⋯`, at every width. Delete especially: a destructive
  // action should not sit one pixel from a view toggle wearing the same plain-text clothes as
  // the thing beside it. What stays on the bar is what you reach for while reading — the mode
  // switch and the bookmark.
  let menu: MenuState | null = $state(null)
  let moreItems = $derived([
    ...(session?.noteOnly || !onShare ? [] : [{ label: 'Share…', run: onShare }]),
    { label: 'Rename / move…', run: onRename },
    { label: '', separator: true },
    { label: 'Move to trash', danger: true, run: onDelete },
  ])

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
        <!-- One dot, two meanings, never at once: on the active tab it marks where you are, and
             on the others it marks a pin. A pinned tab is also the one without a `×`, so an
             active pinned tab still says so. -->
        {#if id === pane.active}<span class="dot"></span>{:else if pinned.includes(id)}<span class="dot pinned" title="Pinned"></span>{/if}
        <span class="label">{isBlank(id) ? 'New tab' : displayName(pathOf(id) ?? id)}</span>
        {#if !pinned.includes(id)}
          <span class="x" role="button" tabindex="-1" aria-label="Close tab" onclick={(e) => { e.stopPropagation(); onClose(id) }} onkeydown={() => {}}>×</span>
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
        <nav class="crumbs" title={activePath}>
          {#if vaultLabel?.(pane.active)}<span class="vault">{vaultLabel(pane.active)}</span><span class="sep">/</span>{/if}
          {#each crumbs.folders as f (f)}<span class="folder">{f}</span><span class="sep">/</span>{/each}
          <span class="name">{crumbs.name}</span>
        </nav>
        {#if presence.length}
          <span class="presence" title={presence.join(', ')}>· with {presence.length === 1 ? presence[0] : `${presence.length} others`}</span>
        {/if}
        <span class="spacer"></span>
        <span class="modes" role="group" aria-label="View mode">
          {#each VIEW_MODES as m (m.id)}
            <button class:on={pane.mode === m.id} onclick={() => onMode?.(m.id)} title={m.hint} aria-pressed={pane.mode === m.id}>{m.label}</button>
          {/each}
        </span>
        <button class="star" onclick={onBookmark} title="Bookmark (Ctrl+Shift+B)">{session.isBookmarked('note', activePath) ? '★' : '☆'}</button>
        <button class="more" onclick={(e) => (menu = menuAt(e, moreItems))} title="Rename, share, delete" aria-label="More actions">···</button>
      </div>
      <div class="editor-wrap">
        <Editor
          {session}
          noteId={pane.active}
          {onOpen}
          onHeadings={(h) => onHeadings?.(h)}
          onPresence={(p) => { presence = p; onPresence?.(p) }}
          onStats={(s) => (stats = s)}
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
      <div class="note-foot">
        <span>{stats.lines} {stats.lines === 1 ? 'line' : 'lines'}</span>
        <span>{stats.words} {stats.words === 1 ? 'word' : 'words'}</span>
        <span class="spacer"></span>
        <span>{format}</span>
      </div>
    {/key}
  {:else}
    <div class="placeholder">
      <p>Open a note from the tree, or press <kbd>Ctrl</kbd>+<kbd>O</kbd>.</p>
    </div>
  {/if}
</section>

{#if menu}
  <ContextMenu {menu} onClose={() => (menu = null)} />
{/if}

<style>
  .pane {
    display: grid;
    /* tabs · header · editor · backlinks · footer */
    grid-template-rows: auto auto minmax(0, 1fr) auto auto;
    min-width: 0;
    min-height: 0;
    flex: 1 1 0;
    border-top: 2px solid transparent;
    /* The header has to fit *this pane*, not the window: two panes side by side on a wide
       monitor are each narrower than a phone, so the window is the wrong thing to ask.
       `flex: 1 1 0` with `min-width: 0` gives the width from the flex line rather than from
       the contents, which is what inline-size containment requires. */
    container-type: inline-size;
    container-name: pane;
  }
  .pane.focused {
    border-top-color: var(--accent);
  }
  /* Tabs sit *on* the chrome and the active one lifts out of it into the document, so the
     strip and the page below read as one surface with a notch cut in it rather than two
     stacked bars. That is what `flex-end` plus the negative margin buy. */
  .tabs {
    display: flex;
    align-items: flex-end;
    gap: 2px;
    padding: 0 0.5rem;
    overflow-x: auto;
    border-bottom: 1px solid var(--border);
    background: var(--chrome);
  }
  .tab {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font: inherit;
    font-size: 0.8125rem;
    border: 1px solid transparent;
    border-bottom: 0;
    background: none;
    color: var(--muted);
    height: 2.125rem;
    padding: 0 0.875rem;
    border-radius: 8px 8px 0 0;
    cursor: pointer;
    white-space: nowrap;
    flex: none;
  }
  .tab:hover:not(.active) {
    color: var(--fg);
    background: var(--hover);
  }
  .tab.active {
    color: var(--fg);
    font-weight: 500;
    background: var(--bg);
    border-color: var(--border);
    /* Over the strip's own bottom border, so the active tab opens into the page. */
    margin-bottom: -1px;
    padding-bottom: 1px;
  }
  .tab.blank {
    font-style: italic;
  }
  .tab .label {
    max-width: 12rem;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tab .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--accent);
    flex: none;
  }
  .tab .dot.pinned {
    background: var(--faint);
  }
  /* Sits after the last tab rather than pinned to the right, the way a browser strip does. */
  .newtab {
    display: grid;
    place-items: center;
    border: 0;
    background: none;
    color: var(--faint);
    height: 2.125rem;
    padding: 0 0.6rem;
    border-radius: 6px;
    cursor: pointer;
    flex: none;
  }
  .newtab:hover {
    color: var(--fg);
    background: var(--hover);
  }
  .tab .x {
    color: var(--faint);
    font-size: 1.05em;
    line-height: 1;
    border-radius: 3px;
  }
  .tab .x:hover {
    color: var(--fg);
    background: var(--hover);
  }

  /* The second and last chrome row: where you are, how you are looking at it, and everything
     else behind `···`. The file path used to have a row of its own above this one. */
  .note-head {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    padding: 0 1rem;
    height: 2.5rem;
    font-size: 0.75rem;
    color: var(--muted);
    border-bottom: 1px solid var(--border-soft);
    /* A pane is as narrow as a third of the window; when the controls stop fitting they take
       a second row instead of being squeezed until their labels clip. */
    flex-wrap: wrap;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
  }
  .crumbs .vault {
    text-transform: uppercase;
    font-size: 0.9em;
    letter-spacing: 0.05em;
  }
  .crumbs .folder,
  .crumbs .vault {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .crumbs .sep {
    color: var(--border);
    flex: none;
  }
  .crumbs .name {
    color: var(--fg);
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .note-head button,
  .backlinks button {
    font: inherit;
    font-size: 0.8rem;
    border: 0;
    background: none;
    color: var(--faint);
    padding: 0.2rem 0.45rem;
    border-radius: 4px;
    cursor: pointer;
  }
  .note-head button:hover {
    color: var(--fg);
    background: var(--hover);
  }
  .note-head .more {
    letter-spacing: 0.06em;
  }
  .spacer {
    flex: 1;
  }
  /* One segmented control, so the three modes read as a choice rather than three buttons. */
  .modes {
    display: inline-flex;
    /* Never shrink: a clipped “Reading” is worse than a wrapped row. */
    flex: none;
    gap: 2px;
    background: var(--chrome);
    border-radius: 6px;
    padding: 2px;
  }
  .modes button {
    border-radius: 4px;
    padding: 0.15rem 0.6rem;
    font-size: 0.7rem;
  }
  /* A raised chip rather than a tinted one: the same "lifted out of the groove" language the
     active tab uses, so both say "selected" the same way. */
  .modes button.on {
    background: var(--bg);
    color: var(--fg);
    font-weight: 600;
    box-shadow: 0 1px 1.5px rgb(0 0 0 / 0.07);
  }

  /* Counts and format, in the quietest type in the app — it is reference, not chrome you act
     on, so nothing in it is a button. */
  .note-foot {
    display: flex;
    align-items: center;
    gap: 1.1rem;
    height: 1.75rem;
    padding: 0 1.25rem;
    border-top: 1px solid var(--border-soft);
    font-size: 0.6875rem;
    color: var(--faint);
    font-variant-numeric: tabular-nums;
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
    padding: 1rem;
    text-align: center;
  }

  /* ---- a narrow pane.

     The actions no longer fold at a breakpoint — they live in `···` at every width — so all
     that is left to give back is the padding. */
  @container pane (max-width: 560px) {
    .note-head,
    .note-foot {
      padding-left: 0.6rem;
      padding-right: 0.6rem;
    }
    .backlinks {
      padding: 0.4rem 0.6rem;
    }
    .tab .label {
      max-width: 8rem;
    }
  }

  /* Then, on a pane no wider than a phone, the row splits in two — the title is the main thing
     telling you where you are, and it should not be sharing its line with a segmented control.
     The spacer already sits exactly where the break belongs, so it becomes the break. */
  @container pane (max-width: 420px) {
    .spacer {
      flex-basis: 100%;
      height: 0;
    }
  }

  /* ---- touch: a finger needs somewhere to land */
  @media (pointer: coarse) {
    .tabs {
      /* Flicking the strip must not drag the note behind it. */
      overscroll-behavior-x: contain;
    }
    /* The tab keeps its shape and grows downwards: the notch it cuts in the strip only works
       while the tab and the strip are the same height. */
    .tab,
    .newtab {
      height: 2.6rem;
      padding-left: 0.9rem;
      padding-right: 0.9rem;
    }
    .tab .x {
      padding: 0 0.3rem;
    }
    .note-head button {
      padding: 0.4rem 0.6rem;
    }
    .modes button {
      padding: 0.35rem 0.6rem;
    }
  }
</style>
