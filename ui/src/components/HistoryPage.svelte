<script lang="ts">
  import { api, type Version } from '../lib/api.ts'
  import { displayName, type VaultSession } from '../lib/vault.svelte.ts'
  import VersionView from './VersionView.svelte'
  import { embedUrlFor } from '../lib/attachments.ts'

  /**
   * A note's history, as a page rather than a panel: the log reads like a note whose content is
   * the list of versions, and picking one replaces the page with that version. It lives in a
   * pane of its own (`PaneState.kind`), so "then" sits beside "now" using the splitting the
   * window already does instead of a third column that is empty most of the time.
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
    onAsk: (title: string, initial: string) => Promise<string | null>
  } = $props()

  let versions: Version[] = $state([])
  let error = $state('')
  let shown: { seq: number; content: string } | null = $state(null)
  let current = $state('')
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

  async function save() {
    const label = await onAsk('Label this version', new Date().toLocaleString())
    if (label === null) return
    await api.saveVersion(session.id, noteId, label || 'saved version').catch((e) => (error = String(e)))
    await reload()
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

  const when = (ms: number) => new Date(ms).toLocaleString()
  let one = $derived(versions.find((v) => v.seq === seq))
</script>

<div class="history">
  <div class="page" class:showing={seq !== 0}>
    {#if seq === 0}
      <div class="head">
        {#each trail as part (part)}<span>{part}</span><span class="sep">/</span>{/each}
        <span>{title}</span>
      </div>
      <h1>History</h1>
      {#if error}<p class="none">{error}</p>{/if}
      <div class="log">
        {#each versions as v (v.seq)}
          <button class="row" onclick={() => onSeq(v.seq)}>
            <span class="label">{v.label ?? 'auto snapshot'}</span>
            <span class="grow"></span>
            {#if v.author}<span class="by">{v.author}</span>{/if}
            <span class="stamp">{when(v.created_ms)}</span>
          </button>
        {/each}
      </div>
      {#if versions.length === 0 && !error}
        <p class="none">No versions yet — snapshots appear as you edit, and <em>Save version…</em> names one.</p>
      {/if}
    {:else}
      <!-- No heading of our own here: the version below carries its own title, rendered from
           its own text. This line is what you can do with it and when it is from. -->
      <div class="head">
        <button class="back" onclick={() => onSeq(0)}>‹ All versions</button>
        <span class="sep">/</span>
        <span class="which">{one?.label ?? 'auto snapshot'}</span>
        <span class="grow"></span>
        <span class="stamp">{one ? when(one.created_ms) : ''}{one?.author ? ` · ${one.author}` : ''}</span>
        <button class="restore" onclick={restore} disabled={!shown}>Restore</button>
      </div>
    {/if}
  </div>
  {#if seq !== 0 && shown}
    <VersionView content={shown.content} {current} embedUrl={(t) => embedUrlFor(session, session.pathOf(noteId) ?? '', t)} />
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
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
  .restore {
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
  .restore:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .log {
    display: flex;
    flex-direction: column;
    margin: 1.4rem -0.5rem 0;
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: 0.75rem;
    width: 100%;
    text-align: left;
    font: inherit;
    border: 0;
    border-top: 1px solid var(--border-soft);
    background: none;
    color: inherit;
    padding: 0.55rem 0.5rem;
    border-radius: 6px;
    cursor: pointer;
  }
  .row:hover {
    background: var(--hover);
  }
  .label {
    min-width: 0;
    font-size: 0.85rem;
    font-weight: 500;
    color: var(--fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
    .row {
      padding-top: 0.7rem;
      padding-bottom: 0.7rem;
    }
  }
</style>
