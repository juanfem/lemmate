<script lang="ts">
  import { api, ApiError, type Changes, type HistoryEntry } from '../lib/api.ts'
  import { displayName, type VaultSession } from '../lib/vault.svelte.ts'
  import VersionView from './VersionView.svelte'
  import DiffView from './DiffView.svelte'
  import { embedUrlFor } from '../lib/attachments.ts'

  /**
   * A note's history, as a page rather than a panel: the log reads like a note whose content is
   * the list of versions, and picking one replaces the page with that version. It lives in a
   * pane of its own (`PaneState.kind`), so "then" sits beside "now" using the splitting the
   * window already does instead of a third column that is empty most of the time.
   *
   * Each row is an editing session or a named version (`lemmate_core::history`), described by
   * its name if it has one and otherwise by what changed and where. A version opens on what it
   * changed; it can also be compared with the note as it is now, or read as it was.
   */
  let {
    session,
    noteId,
    seq,
    onSeq,
    onAsk,
  }: {
    session: VaultSession
    noteId: string
    /** Which version is on the page; 0 is the log. */
    seq: number
    onSeq: (seq: number) => void
    onAsk: (title: string, initial: string, opts?: { placeholder?: string }) => Promise<string | null>
  } = $props()

  type Mode = 'changes' | 'since' | 'read'

  let versions: HistoryEntry[] = $state([])
  let error = $state('')
  let shown: { seq: number; content: string } | null = $state(null)
  /** The entry before the one shown, as text: '' when the shown one is where the note began,
   *  null while it loads or when older history has been pruned. */
  let previous: string | null = $state(null)
  let current = $state('')
  let mode = $state<Mode>('changes')
  let title = $derived(displayName(session.pathOf(noteId) ?? ''))
  let trail = $derived((session.pathOf(noteId) ?? '').split('/').slice(0, -1))

  async function reload() {
    try {
      versions = await api.versions(session.id, noteId)
      error = ''
    } catch {
      versions = []
      error = 'History is not available here.'
    }
  }

  $effect(() => {
    noteId
    void reload()
  })

  // The note as it stands, for marking what a version no longer matches. Read and released at
  // once: a history pane watches nothing, it only needs the text that is there now.
  $effect(() => {
    const id = noteId
    const { doc, release } = session.acquire(id)
    current = doc.getText('content').toString()
    release()
  })

  $effect(() => {
    const want = seq
    if (want === 0) {
      shown = null
      return
    }
    let live = true
    api
      .versionAt(session.id, noteId, want)
      .then((v) => live && (shown = v))
      .catch(() => live && (shown = null))
    return () => {
      live = false
    }
  })

  let index = $derived(versions.findIndex((v) => v.seq === seq))
  let one = $derived(index >= 0 ? versions[index] : undefined)
  /** Whether the shown entry has anything to be compared with (not the oldest of a pruned log). */
  let hasPrevious = $derived(!!one && one.changes !== null)

  $effect(() => {
    const i = index
    const list = versions
    previous = null
    if (i < 0 || list[i]!.changes === null) return
    if (i === list.length - 1) {
      previous = ''
      return
    }
    let live = true
    api
      .versionAt(session.id, noteId, list[i + 1]!.seq)
      .then((v) => live && (previous = v.content))
      .catch(() => {})
    return () => {
      live = false
    }
  })

  // An entry with nothing before it cannot show what it changed; show it against now instead.
  let view: Mode = $derived(mode === 'changes' && one && !hasPrevious ? 'since' : mode)

  const refusal = (e: unknown) =>
    e instanceof ApiError && e.status === 403 ? 'You can read this history but not name its versions.' : String(e)

  async function save() {
    const label = await onAsk('Save a version', '', { placeholder: 'What it is, e.g. “sent for review”' })
    if (label === null) return
    try {
      await api.saveVersion(session.id, noteId, label.trim() || 'saved version')
      await reload()
    } catch (e) {
      // The note is as the last named version left it: a save would only rename that one.
      error =
        e instanceof ApiError && e.status === 409
          ? 'Nothing has changed since the last saved version — rename it instead (✎).'
          : refusal(e)
    }
  }

  /** Name, rename or (an empty name) unname a version; a named one is kept forever. */
  async function rename(v: HistoryEntry) {
    const typed = await onAsk(v.label ? 'Rename version' : 'Name this version', v.label ?? '', {
      placeholder: 'Leave empty to remove the name',
    })
    if (typed === null) return
    const label = typed.trim() || null
    if (label === v.label) return
    try {
      await api.labelVersion(session.id, noteId, v.seq, label)
      await reload()
    } catch (e) {
      error = refusal(e)
    }
  }

  /** Restore is one more edit that sets the text back; history keeps everything (SPEC §9). */
  function restore() {
    if (!shown) return
    const { doc, release } = session.acquire(noteId)
    const text = doc.getText('content')
    doc.transact(() => {
      text.delete(0, text.length)
      text.insert(0, shown!.content)
    })
    release()
    onSeq(0)
  }

  const time = (ms: number) => new Date(ms).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' })
  const day = (ms: number) =>
    new Date(ms).toLocaleDateString(undefined, { weekday: 'short', year: 'numeric', month: 'short', day: 'numeric' })
  /** When a session ran: from its first edit to its last snapshot, if that is a span at all. */
  function span(v: HistoryEntry) {
    if (v.created_ms - v.started_ms < 60_000) return time(v.created_ms)
    if (day(v.started_ms) === day(v.created_ms)) return `${time(v.started_ms)}–${time(v.created_ms)}`
    // Begun on an earlier day: the entry sits under the day it ended, so name the first one.
    const from = new Date(v.started_ms).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
    return `since ${from} ${time(v.started_ms)} – ${time(v.created_ms)}`
  }

  /** Where the changes are, in a few words. */
  function where(c: Changes | null, first: boolean): string {
    if (c === null) return 'Oldest version kept'
    if (first) return 'Created'
    if (c.sections.length === 0) return c.added + c.removed === 0 ? 'No text changes' : 'Edits'
    const shownSections = c.sections.slice(0, 3).join(' · ')
    return c.sections.length > 3 ? `${shownSections} +${c.sections.length - 3} more` : shownSections
  }

  /** The log, grouped under the day each entry ended on. */
  let days = $derived.by(() => {
    const out: { day: string; entries: { v: HistoryEntry; first: boolean }[] }[] = []
    versions.forEach((v, i) => {
      const d = day(v.created_ms)
      const first = i === versions.length - 1
      if (out.at(-1)?.day !== d) out.push({ day: d, entries: [] })
      out.at(-1)!.entries.push({ v, first })
    })
    return out
  })
</script>

<div class="history">
  <div class="page" class:showing={seq !== 0}>
    {#if seq === 0}
      <div class="head">
        {#each trail as part (part)}<span>{part}</span><span class="sep">/</span>{/each}
        <span>{title}</span>
      </div>
      <div class="title">
        <h1>History</h1>
        <button class="action" onclick={save} title="Keep the note as it is now, under a name, for good">Save version…</button>
      </div>
      {#if error}<p class="none">{error}</p>{/if}
      {#each days as group (group.day)}
        <h2 class="day">{group.day}</h2>
        <div class="log">
          {#each group.entries as { v, first } (v.seq)}
            <div class="row" class:named={v.label}>
              <button class="open" onclick={() => onSeq(v.seq)}>
                {#if v.label}
                  <span class="label">{v.label}</span>
                  <span class="where">{where(v.changes, first)}</span>
                {:else}
                  <span class="label">{where(v.changes, first)}</span>
                {/if}
                {#if v.changes && v.changes.added + v.changes.removed > 0}
                  <span class="delta"
                    >{#if v.changes.added}<span class="add-n">+{v.changes.added}</span>{/if}{#if v.changes.removed}<span class="del-n"
                        >−{v.changes.removed}</span
                      >{/if}</span
                  >
                {/if}
                <span class="grow"></span>
                {#if v.author}<span class="by">{v.author}</span>{/if}
                <span class="stamp">{span(v)}</span>
              </button>
              <button class="name" onclick={() => rename(v)} title={v.label ? 'Rename' : 'Name this version'} aria-label={v.label ? 'Rename' : 'Name this version'}
                >✎</button
              >
            </div>
          {/each}
        </div>
      {/each}
      {#if versions.length === 0 && !error}
        <p class="none">No versions yet — they appear as you edit, and <em>Save version…</em> keeps one under a name.</p>
      {/if}
    {:else}
      <!-- No heading of our own here: the version below carries its own title, rendered from
           its own text. This line is what you can do with it and when it is from. -->
      <div class="head">
        <button class="back" onclick={() => onSeq(0)}>‹ All versions</button>
        <span class="sep">/</span>
        {#if one}
          <button class="which" onclick={() => rename(one!)} title={one.label ? 'Rename' : 'Name this version'}
            >{one.label ?? where(one.changes, index === versions.length - 1)} <span class="pen">✎</span></button
          >
        {/if}
        <span class="grow"></span>
        <span class="stamp">{one ? `${day(one.created_ms)} ${span(one)}` : ''}{one?.author ? ` · ${one.author}` : ''}</span>
        <button class="action" onclick={restore} disabled={!shown}>Restore</button>
      </div>
      {#if error}<p class="none">{error}</p>{/if}
      <div class="modes" role="tablist">
        <button role="tab" aria-selected={view === 'changes'} class:on={view === 'changes'} disabled={!hasPrevious} onclick={() => (mode = 'changes')}
          title={hasPrevious ? 'What this version changed' : 'Nothing older is kept to compare with'}>What changed</button
        >
        <button role="tab" aria-selected={view === 'since'} class:on={view === 'since'} onclick={() => (mode = 'since')}
          title="From this version to the note as it is now">Compared with now</button
        >
        <button role="tab" aria-selected={view === 'read'} class:on={view === 'read'} onclick={() => (mode = 'read')}
          title="This version as it read">Read</button
        >
      </div>
    {/if}
  </div>
  {#if seq !== 0 && shown}
    {#if view === 'read'}
      <VersionView content={shown.content} {current} embedUrl={(t) => embedUrlFor(session, session.pathOf(noteId) ?? '', t)} />
    {:else if view === 'since'}
      <DiffView before={shown.content} after={current} />
    {:else if previous !== null}
      <DiffView before={previous} after={shown.content} />
    {/if}
  {/if}
</div>

<style>
  .history {
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: auto;
  }
  /* The same measure the note keeps, so the log reads as a page of the same book. */
  .page {
    width: 100%;
    max-width: 42.5rem;
    margin: 0 auto;
    padding: 2.75rem clamp(0.9rem, 4vw, 2.5rem) 0;
    flex: none;
  }
  /* A version is not a page of its own: it is the note, from before. So the page's padding
     goes with the log, and what is left above a version is one line of chrome. */
  .page.showing {
    padding-top: 0.85rem;
    padding-bottom: 0.35rem;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.72rem;
    line-height: 1.4;
    color: var(--faint);
    padding-bottom: 0.35rem;
  }
  .sep {
    color: var(--border);
  }
  .grow {
    flex: 1;
  }
  .which {
    min-width: 0;
    font: inherit;
    border: 0;
    background: none;
    padding: 0;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
  }
  .which:hover {
    color: var(--fg);
  }
  .pen {
    color: var(--faint);
  }
  .title {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  .title h1 {
    flex: 1;
  }
  .day {
    margin: 1.6rem 0 0.3rem;
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--faint);
  }
  .modes {
    display: flex;
    gap: 0.25rem;
    padding: 0.2rem 0 0.1rem;
  }
  .modes button {
    font: inherit;
    font-size: 0.75rem;
    border: 1px solid transparent;
    background: none;
    color: var(--muted);
    border-radius: 6px;
    padding: 0.2rem 0.6rem;
    cursor: pointer;
  }
  .modes button.on {
    border-color: var(--border);
    background: var(--panel);
    color: var(--fg);
  }
  .modes button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .back {
    font: inherit;
    border: 0;
    background: none;
    padding: 0;
    color: var(--accent);
    cursor: pointer;
  }
  h1 {
    margin: 0;
    font-family: var(--prose);
    font-size: 1.9em;
    line-height: 1.3;
    font-weight: 600;
    color: var(--prose-fg);
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .action {
    flex: none;
    font: inherit;
    font-size: 0.75rem;
    border: 1px solid var(--border);
    background: var(--bg);
    /* The one thing on this line you can act on, so it does not inherit the line's grey. */
    color: var(--fg);
    border-radius: 6px;
    padding: 0.2rem 0.6rem;
    cursor: pointer;
  }
  .action:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .log {
    display: flex;
    flex-direction: column;
    margin: 0 -0.5rem;
  }
  .row {
    display: flex;
    align-items: center;
    border-top: 1px solid var(--border-soft);
    border-radius: 6px;
  }
  .row:hover {
    background: var(--hover);
  }
  .open {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 0.6rem;
    text-align: left;
    font: inherit;
    border: 0;
    background: none;
    color: inherit;
    padding: 0.55rem 0 0.55rem 0.5rem;
    cursor: pointer;
  }
  .label {
    min-width: 0;
    font-size: 0.85rem;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .named .label {
    font-weight: 600;
    color: var(--fg);
  }
  .where {
    min-width: 0;
    flex: 0 1 auto;
    font-size: 0.75rem;
    color: var(--faint);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .delta {
    flex: none;
    display: flex;
    gap: 0.35rem;
    font-size: 0.72rem;
    font-variant-numeric: tabular-nums;
  }
  .add-n {
    color: var(--ok);
  }
  .del-n {
    color: var(--danger);
  }
  .name {
    flex: none;
    font: inherit;
    font-size: 0.8rem;
    border: 0;
    background: none;
    color: var(--faint);
    padding: 0.3rem 0.55rem;
    border-radius: 6px;
    cursor: pointer;
    opacity: 0;
  }
  .row:hover .name,
  .name:focus-visible {
    opacity: 1;
  }
  .name:hover {
    color: var(--fg);
  }
  .by,
  .stamp {
    flex: none;
    font-size: 0.72rem;
    color: var(--faint);
  }
  .stamp {
    font-variant-numeric: tabular-nums;
  }
  .none {
    margin: 1rem 0 0;
    font-size: 0.8rem;
    line-height: 1.5;
    color: var(--faint);
  }

  @media (pointer: coarse) {
    .open {
      padding-top: 0.7rem;
      padding-bottom: 0.7rem;
    }
    /* No hover to reveal it with. */
    .name {
      opacity: 1;
    }
  }
</style>
