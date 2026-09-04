<script lang="ts" module>
  import type { ViewMode } from '../lib/editor/setup.ts'

  /** One editing pane: its own tab strip, page and footer (SPEC §9). */
  export interface PaneState {
    id: number
    tabs: string[]
    active: string | null
    /** SPEC §8 view mode, per pane — a source pane can sit beside a reading one. */
    mode: ViewMode
    /** A history pane shows a note's versions instead of its text; absent means the note. */
    kind?: 'note' | 'history'
    /** Which version a history pane is showing; 0, or absent, is the log itself. */
    seq?: number
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
  import { displayName, type VaultSession } from '../lib/vault.svelte.ts'
  import Editor from './Editor.svelte'
  import HistoryPage from './HistoryPage.svelte'
  import { VIEW_MODES } from '../lib/editor/setup.ts'
  import Icon from './Icon.svelte'
  import ContextMenu, { menuAt, type MenuState } from './ContextMenu.svelte'
  import type { OutlineItem } from '../lib/outline.ts'

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
    onPresence,
    onMode,
    onNewTab,
    onSplit,
    splitFull = false,
    onClosePane,
    onHistory,
    historyOpen = false,
    onSeq,
    onAsk,
    jumpTo = $bindable(),
  }: {
    /** Which vault a tab belongs to; tabs in one pane may come from different vaults. */
    lookup: (noteId: string) => VaultSession | undefined
    /** Vault label for the folder trail, empty when there is only one vault to speak of. */
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
    onPresence?: (names: string[]) => void
    onMode?: (mode: ViewMode) => void
    onNewTab?: () => void
    /** Splitting right, on the strip rather than only on Ctrl+\ and in the palette. */
    onSplit?: () => void
    /** Three panes is the limit: the control stays and says so rather than disappearing. */
    splitFull?: boolean
    /** Absent in the last pane — there is nothing to close back to. */
    onClosePane?: () => void
    onHistory?: () => void
    /** Whether this note's history already has a pane, so the clock can say so. */
    historyOpen?: boolean
    onSeq?: (seq: number) => void
    onAsk?: (title: string, initial: string) => Promise<string | null>
    jumpTo?: (pos: number) => void
  } = $props()

  let host: HTMLElement
  let presence: string[] = $state([])
  let headings: OutlineItem[] = $state([])
  /**
   * The index skips the note's own title. A margin index answers "where am I in this note",
   * and the title is not somewhere you can be — it is the line the reader is already looking
   * at. A note that uses `#` for several sections still lists every one of them.
   */
  let index = $derived(headings[0]?.level === 1 ? headings.slice(1) : headings)
  /** Where the reader is in the note, as the editor last reported it. */
  let here = $state(0)
  /** The section the top of the viewport is inside: the last heading at or before it. Before
   *  the first one — in the title, or the lines under it — nothing is current, which is true. */
  let current = $derived(index.reduce((best, h, i) => (h.pos <= here ? i : best), -1))

  let isHistory = $derived(pane.kind === 'history')

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

  /** Where the note lives — vault, then folders. Its name is the heading below, not here. */
  let trail = $derived.by(() => {
    if (!pane.active) return []
    const label = vaultLabel?.(pane.active)
    return [...(label ? [label] : []), ...activePath.split('/').slice(0, -1).filter(Boolean)]
  })
  /** Quarto notes are still markdown, but saying so is the point of the label. */
  let format = $derived(activePath.endsWith('.qmd') ? 'Quarto' : 'Markdown')
  let stats: { lines: number; words: number } = $state({ lines: 0, words: 0 })

  // What the strip folds away on a narrow pane lives here too, so nothing becomes unreachable
  // by making the window smaller. Delete stays at the bottom, behind a separator: a destructive
  // action should not sit one pixel from a view toggle wearing the same clothes.
  let menu: MenuState | null = $state(null)
  let bookmarked = $derived(!!session?.isBookmarked('note', activePath))
  let moreItems = $derived([
    ...(onHistory ? [{ label: 'Version history', run: onHistory }] : []),
    { label: bookmarked ? 'Remove bookmark' : 'Bookmark this note', run: onBookmark },
    ...(session?.noteOnly || !onShare ? [] : [{ label: 'Share…', run: onShare }]),
    { label: 'Rename / move…', run: onRename },
    { label: '', separator: true },
    { label: 'Move to trash', danger: true, run: onDelete },
  ])

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
  <!-- One row of chrome, not two: the tabs say which note, and everything you do *to* the note
       you are looking at sits at the other end of the same strip. Where the note lives moved on
       to the page itself (lib/editor/page.ts), which is what freed the second row. -->
  <div class="tabs">
    {#each tabs as id (id)}
      <button class="tab" class:active={id === pane.active} class:blank={isBlank(id)} onclick={() => onActivate(id)} title={pathOf(id)}>
        {#if isHistory}
          <Icon name="history" size={13} />
        {:else if id === pane.active}
          <!-- One dot, two meanings, never at once: on the active tab it marks where you are,
               and on the others it marks a pin. A pinned tab is also the one without a `×`. -->
          <span class="dot"></span>
        {:else if pinned.includes(id)}
          <span class="dot pinned" title="Pinned"></span>
        {/if}
        <span class="label">{isBlank(id) ? 'New tab' : displayName(pathOf(id) ?? id)}{isHistory ? ' · history' : ''}</span>
        {#if isHistory}
          <span class="x" role="button" tabindex="-1" aria-label="Close pane" onclick={(e) => { e.stopPropagation(); onClosePane?.() }} onkeydown={() => {}}>×</span>
        {:else if !pinned.includes(id)}
          <span class="x" role="button" tabindex="-1" aria-label="Close tab" onclick={(e) => { e.stopPropagation(); onClose(id) }} onkeydown={() => {}}>×</span>
        {/if}
      </button>
    {/each}
    {#if onNewTab && !isHistory}
      <button class="newtab" onclick={onNewTab} title="New tab (Ctrl+T)" aria-label="New tab"><Icon name="plus" size={13} /></button>
    {/if}
    <span class="spacer"></span>
    <div class="cluster">
      {#if !isHistory && pane.active && session && !isBlank(pane.active)}
        <span class="modes fold" role="group" aria-label="View mode">
          {#each VIEW_MODES as m (m.id)}
            <button class:on={pane.mode === m.id} onclick={() => onMode?.(m.id)} title={m.hint} aria-pressed={pane.mode === m.id}>{m.label}</button>
          {/each}
        </span>
        <button class="icon fold" class:on={bookmarked} onclick={onBookmark} title="Bookmark (Ctrl+Shift+B)" aria-label="Bookmark" aria-pressed={bookmarked}>
          <Icon name="star" size={15} filled={bookmarked} />
        </button>
        {#if onHistory}
          <button class="icon fold" class:on={historyOpen} onclick={onHistory} title="Version history" aria-label="Version history" aria-pressed={historyOpen}>
            <Icon name="history" size={15} />
          </button>
        {/if}
      {/if}
      {#if onSplit && !isHistory}
        <button class="icon" onclick={onSplit} disabled={splitFull} title={splitFull ? 'Three panes is the limit' : 'Split right (Ctrl+\\)'} aria-label="Split right">
          <Icon name="splitright" size={15} />
        </button>
      {/if}
      {#if onClosePane && !isHistory}
        <button class="icon" onclick={onClosePane} title="Close pane" aria-label="Close pane"><Icon name="closepane" size={15} /></button>
      {/if}
      {#if !isHistory}
        <button class="icon more" onclick={(e) => (menu = menuAt(e, moreItems))} title="History, bookmark, rename, delete" aria-label="More actions">···</button>
      {/if}
    </div>
  </div>

  {#if isHistory && pane.active && session && onSeq && onAsk}
    {#key pane.active}
      <HistoryPage {session} noteId={pane.active} seq={pane.seq ?? 0} {onSeq} {onAsk} />
    {/key}
    <div class="note-foot">
      <span>{displayName(activePath)}</span>
      <span class="spacer"></span>
      <span>version history</span>
    </div>
  {:else if pane.active && session && !isBlank(pane.active)}
    {#key pane.active}
      <div class="page">
        <!-- The outline in the margin the measure already leaves empty. It is an index, not a
             panel: no header, no chrome, and gone the moment the pane is too narrow to hold a
             margin at all — at which point ⌘K and the headings themselves are how you move. -->
        {#if index.length}
          <nav class="margin" aria-label="Outline">
            {#each index as h, i (h.pos)}
              <button
                class="entry"
                class:deep={h.level > 1}
                class:current={i === current}
                aria-current={i === current ? 'true' : undefined}
                onclick={() => jumpTo?.(h.pos)}
                title={h.text}>{h.text}</button>
            {/each}
          </nav>
        {/if}
        <div class="editor-wrap">
          <Editor
            {session}
            noteId={pane.active}
            {onOpen}
            {trail}
            onHeadings={(h) => (headings = h)}
            onHere={(p) => (here = p)}
            onPresence={(p) => { presence = p; onPresence?.(p) }}
            onStats={(s) => (stats = s)}
            mode={pane.mode}
            bind:jumpTo
          />
        </div>
      </div>
      <div class="note-foot">
        <span>{stats.lines} {stats.lines === 1 ? 'line' : 'lines'}</span>
        <span>{stats.words} {stats.words === 1 ? 'word' : 'words'}</span>
        {#if presence.length}
          <span class="presence" title={presence.join(', ')}>with {presence.length === 1 ? presence[0] : `${presence.length} others`}</span>
        {/if}
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
    /* tabs · page · footer */
    grid-template-rows: auto minmax(0, 1fr) auto;
    min-width: 0;
    min-height: 0;
    flex: 1 1 0;
    border-top: 2px solid transparent;
    /* The chrome has to fit *this pane*, not the window: two panes side by side on a wide
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
     stacked bars. That is what `flex-end` plus the negative margin buy. The strip is `--panel`
     rather than `--chrome` so the groove of the mode switch, which is `--chrome`, still reads
     as a groove when it sits on it. */
  .tabs {
    display: flex;
    align-items: flex-end;
    gap: 2px;
    padding: 0 0.5rem;
    overflow-x: auto;
    border-bottom: 1px solid var(--border);
    background: var(--panel);
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
  .spacer {
    flex: 1;
    min-width: 0.5rem;
  }
  /* The other end of the strip: how you are looking at the note, and what you do to it. It
     sticks so a strip full of tabs scrolls *under* it rather than pushing it off the edge. */
  .cluster {
    display: flex;
    align-items: center;
    gap: 0.15rem;
    flex: none;
    align-self: center;
    position: sticky;
    right: 0;
    padding-left: 0.4rem;
    background: var(--panel);
  }
  .cluster .icon {
    display: grid;
    place-items: center;
    font: inherit;
    font-size: 0.8rem;
    border: 0;
    background: none;
    color: var(--faint);
    padding: 0.25rem;
    border-radius: 4px;
    cursor: pointer;
  }
  .cluster .icon:hover:not(:disabled) {
    color: var(--fg);
    background: var(--hover);
  }
  .cluster .icon:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .cluster .icon.on {
    color: var(--accent);
    background: var(--accent-bg);
  }
  .cluster .more {
    letter-spacing: 0.06em;
    padding: 0.25rem 0.35rem;
  }
  /* One segmented control, so the three modes read as a choice rather than three buttons. */
  .modes {
    display: inline-flex;
    flex: none;
    gap: 2px;
    background: var(--chrome);
    border-radius: 6px;
    padding: 2px;
    margin-right: 0.2rem;
  }
  .modes button {
    font: inherit;
    border: 0;
    background: none;
    color: var(--muted);
    border-radius: 4px;
    padding: 0.15rem 0.6rem;
    font-size: 0.7rem;
    cursor: pointer;
  }
  .modes button:hover:not(.on) {
    color: var(--fg);
  }
  /* A raised chip rather than a tinted one: the same "lifted out of the groove" language the
     active tab uses, so both say "selected" the same way. */
  .modes button.on {
    background: var(--bg);
    color: var(--fg);
    font-weight: 600;
    box-shadow: 0 1px 1.5px rgb(0 0 0 / 0.07);
  }

  /* The page: the editor, and the margin index laid over the empty column beside its measure. */
  .page {
    position: relative;
    min-height: 0;
    display: grid;
  }
  .editor-wrap {
    min-height: 0;
  }
  /* Half the pane minus half the measure (42.5rem in `setup.ts`) is exactly the empty column
     the centred text leaves behind, which is where a marginal index belongs. */
  .margin {
    position: absolute;
    inset: 0 auto 0 0;
    width: calc(50% - 21.25rem);
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 0.15rem;
    padding: 4.15rem 1.2rem 1rem 0.75rem;
    overflow: auto;
    /* CodeMirror's editor is positioned too and comes later in the document, so without a layer
       of its own the index would be painted over and every entry would be unclickable. */
    z-index: 1;
    /* The column is mostly empty paper; a click in it should reach the editor underneath. */
    pointer-events: none;
  }
  .margin .entry {
    pointer-events: auto;
    max-width: 100%;
    font: inherit;
    font-size: 0.72rem;
    line-height: 1.5;
    text-align: right;
    border: 0;
    /* The rule the current entry lights up, held open by every entry so the column never
       shifts sideways as you scroll. */
    border-right: 2px solid transparent;
    background: none;
    color: var(--muted);
    padding: 0.1rem 0.55rem 0.1rem 0;
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .margin .entry.deep {
    font-size: 0.68rem;
    color: var(--faint);
  }
  /* Where the reader is. Marked in the margin rather than by weight alone: at this size a bold
     word is a smudge, and a rule beside the text reads as a position in the note. */
  .margin .entry.current {
    color: var(--fg);
    border-right-color: var(--accent);
  }
  .margin .entry:hover {
    color: var(--accent);
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
  .placeholder {
    display: grid;
    place-items: center;
    color: var(--muted);
    padding: 1rem;
    text-align: center;
  }

  /* ---- a narrow pane.
     First the padding goes, then the margin index (there is no margin left to put it in), and
     then the controls that have a home in `⋯` anyway. */
  @container pane (max-width: 940px) {
    .margin {
      display: none;
    }
  }
  @container pane (max-width: 560px) {
    .note-foot {
      padding-left: 0.6rem;
      padding-right: 0.6rem;
    }
    .tab .label {
      max-width: 8rem;
    }
  }
  @container pane (max-width: 520px) {
    .cluster .fold {
      display: none;
    }
  }

  /* ---- touch: no hover to reveal anything, and a finger is not a pixel */
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
    .cluster .icon {
      padding: 0.45rem;
    }
    .modes button {
      padding: 0.35rem 0.6rem;
    }
  }
</style>
