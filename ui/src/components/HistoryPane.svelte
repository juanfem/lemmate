<script lang="ts">
  import { api, type Version } from '../lib/api.ts'
  import type { VaultSession } from '../lib/vault.svelte.ts'

  let { session, noteId, onAsk }: { session: VaultSession; noteId: string | null; onAsk: (title: string, initial: string) => Promise<string | null> } = $props()
  let versions: Version[] = $state([])
  let preview: { seq: number; content: string } | null = $state(null)
  let error = $state('')

  async function reload() {
    preview = null
    if (!noteId) return (versions = [])
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
    reload()
  })

  async function save() {
    if (!noteId) return
    const label = await onAsk('Label this version', new Date().toLocaleString())
    if (label === null) return
    await api.saveVersion(session.id, noteId, label || 'saved version').catch((e) => (error = String(e)))
    reload()
  }
  async function show(v: Version) {
    if (!noteId) return
    preview = await api.versionAt(session.id, noteId, v.seq).catch(() => null)
  }
  /** Restore = one more edit that sets the text back; history keeps everything (SPEC §9). */
  function restore() {
    if (!noteId || !preview) return
    const { doc, release } = session.acquire(noteId)
    const text = doc.getText('content')
    doc.transact(() => {
      text.delete(0, text.length)
      text.insert(0, preview!.content)
    })
    release()
    preview = null
  }
  const when = (ms: number) => new Date(ms).toLocaleString()
  const body = (t: string) => (t.startsWith('---\n') ? t.slice(t.indexOf('\n---', 4) + 4).replace(/^\n/u, '') : t)
</script>

<div class="history">
  {#if !noteId}
    <p class="muted">Open a note to see its history.</p>
  {:else}
    <div class="bar">
      <button onclick={save}>Save version…</button>
      <button onclick={reload} title="Refresh">↻</button>
    </div>
    {#if error}<p class="muted">{error}</p>{/if}
    <ul>
      {#each versions as v (v.seq)}
        <li class:on={preview?.seq === v.seq}>
          <button onclick={() => show(v)}>
            <span class="label">{v.label ?? 'auto snapshot'}</span>
            <span class="meta">{when(v.created_ms)}{#if v.author} · {v.author}{/if}</span>
          </button>
        </li>
      {/each}
      {#if versions.length === 0 && !error}<li class="muted">No versions yet — snapshots appear as you edit; save one to name it.</li>{/if}
    </ul>
    {#if preview}
      <div class="preview">
        <div class="bar">
          <strong>Version {preview.seq}</strong>
          <span class="spacer"></span>
          <button onclick={restore}>Restore</button>
          <button onclick={() => (preview = null)}>Close</button>
        </div>
        <pre>{body(preview.content)}</pre>
      </div>
    {/if}
  {/if}
</div>

<style>
  .history { display: flex; flex-direction: column; min-height: 0; overflow: auto; font-size: 0.85rem; }
  .bar { display: flex; gap: 0.3rem; padding: 0.4rem; align-items: center; }
  .bar button { font: inherit; font-size: 0.8rem; border: 1px solid var(--border); background: var(--bg); color: inherit; border-radius: 6px; padding: 0.2rem 0.6rem; cursor: pointer; }
  .spacer { flex: 1; }
  ul { list-style: none; margin: 0; padding: 0 0.4rem; }
  li button { width: 100%; display: flex; flex-direction: column; align-items: flex-start; border: 0; background: none; color: inherit; font: inherit; padding: 0.3rem 0.4rem; border-radius: 6px; cursor: pointer; text-align: left; }
  li button:hover, li.on button { background: var(--hover); }
  .label { font-weight: 600; }
  .meta { color: var(--muted); font-size: 0.8em; }
  .preview { border-top: 1px solid var(--border); margin-top: 0.4rem; }
  pre { white-space: pre-wrap; font-family: var(--prose); font-size: 0.85rem; padding: 0 0.6rem 0.6rem; margin: 0; max-height: 40vh; overflow: auto; }
  .muted { color: var(--muted); padding: 0.4rem; }
</style>
