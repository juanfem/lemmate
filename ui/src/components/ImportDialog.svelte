<script lang="ts">
  import { untrack } from 'svelte'
  import { api, type ImportReport } from '../lib/api.ts'
  import { batches, toUploads, totalBytes, type Upload } from '../lib/import.ts'
  import { ulid } from '../lib/ulid.ts'

  /**
   * Obsidian import (SPEC §11.4). You pick the vault folder; the browser uploads it in batches
   * to `POST /vaults/{v}/import`, where the same Rust conversion `lemmate import obsidian` runs
   * turns callouts into fenced divs and `![[img]]` embeds into images. Batching keeps each
   * request under the server's body limit and gives us a progress bar; the endpoint skips paths
   * the vault already has, so a retry cannot duplicate notes.
   */
  let {
    vaults,
    target: initialTarget,
    onClose,
    onImported,
  }: {
    vaults: { id: string; label: string }[]
    target?: string | null
    onClose: () => void
    onImported: (vault: string) => void
  } = $props()

  const NEW_VAULT = 'new'
  // The dialog is created fresh each time it opens, so the target only needs its opening value.
  let target = $state(untrack(() => initialTarget ?? vaults[0]?.id ?? NEW_VAULT))
  let picked: Upload[] = $state([])
  let rootName = $state('')
  let busy = $state(false)
  let done: ImportReport | null = $state(null)
  let error = $state('')
  let sentBytes = $state(0)
  let pickedBytes = $derived(totalBytes(picked))
  let progress = $derived(pickedBytes > 0 ? Math.min(100, Math.round((sentBytes / pickedBytes) * 100)) : 0)

  function pick(e: Event) {
    const input = e.currentTarget as HTMLInputElement
    done = null
    error = ''
    sentBytes = 0
    const { uploads, root } = toUploads([...(input.files ?? [])])
    rootName = root
    picked = uploads
  }

  async function run() {
    if (!picked.length || busy) return
    busy = true
    error = ''
    sentBytes = 0
    const vault = target === NEW_VAULT ? ulid() : target
    const total: ImportReport = {
      notes: 0,
      attachments: 0,
      callouts: 0,
      embeds: 0,
      skipped: 0,
      bookmarks: 0,
      daily_notes: false,
    }
    try {
      // Settings and small files first is not required — the endpoint is order-independent —
      // but sending notes before attachments means the tree fills in while the bytes flow.
      const ordered = [...picked].sort((a, b) => a.file.size - b.file.size)
      for (const batch of batches(ordered)) {
        const r = await api.importBatch(vault, batch)
        total.notes += r.notes
        total.attachments += r.attachments
        total.callouts += r.callouts
        total.embeds += r.embeds
        total.skipped += r.skipped
        total.bookmarks += r.bookmarks
        total.daily_notes ||= r.daily_notes
        sentBytes += batch.reduce((n, f) => n + f.file.size, 0)
      }
      done = total
      onImported(vault)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    } finally {
      busy = false
    }
  }
</script>

<div class="backdrop" onmousedown={() => !busy && onClose()} role="presentation">
  <div class="dialog" onmousedown={(e) => e.stopPropagation()} role="dialog" aria-label="Import an Obsidian vault" tabindex="-1">
    <h2>Import an Obsidian vault</h2>

    {#if done}
      <p class="report">
        Imported <strong>{done.notes}</strong>
        {done.notes === 1 ? 'note' : 'notes'} and <strong>{done.attachments}</strong>
        {done.attachments === 1 ? 'attachment' : 'attachments'}.
      </p>
      <p class="muted">
        {done.callouts} callouts converted, {done.embeds} image embeds rewritten{#if done.bookmarks}, {done.bookmarks} bookmarks kept{/if}{#if done.daily_notes}, daily-note settings kept{/if}.
        {#if done.skipped}<br />{done.skipped} file{done.skipped === 1 ? '' : 's'} skipped — the vault already had them.{/if}
      </p>
      <div class="row end">
        <button class="primary" onclick={onClose}>Done</button>
      </div>
    {:else}
      <label class="field">
        <span>Import into</span>
        <select bind:value={target} disabled={busy}>
          {#each vaults as v (v.id)}
            <option value={v.id}>{v.label}</option>
          {/each}
          <option value={NEW_VAULT}>A new vault</option>
        </select>
      </label>

      <label class="field">
        <span>Vault folder</span>
        <!-- svelte-ignore a11y_no_noninteractive_element_to_interactive_role -->
        <input type="file" webkitdirectory multiple disabled={busy} onchange={pick} />
      </label>

      {#if picked.length}
        <p class="muted">
          {picked.length} files{#if rootName}
            from <code>{rootName}/</code>{/if} · {(pickedBytes / 1024 / 1024).toFixed(1)} MB
        </p>
      {:else}
        <p class="muted">
          Pick the folder that holds your notes. Callouts and image embeds are converted;
          <code>.obsidian/</code> is skipped apart from bookmarks and daily-note settings.
        </p>
      {/if}

      {#if busy}
        <div class="bar"><div class="fill" style:width="{progress}%"></div></div>
      {/if}
      {#if error}<p class="error">{error}</p>{/if}

      <div class="row end">
        <button onclick={onClose} disabled={busy}>Cancel</button>
        <button class="primary" onclick={run} disabled={busy || picked.length === 0}>
          {busy ? `Importing… ${progress}%` : 'Import'}
        </button>
      </div>
    {/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.3);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 12vh;
    z-index: 10;
  }
  .dialog {
    width: min(34rem, 92vw);
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: 0 10px 40px rgb(0 0 0 / 0.3);
    padding: 1rem 1.2rem 1.2rem;
  }
  h2 {
    margin: 0 0 0.8rem;
    font-size: 1.1rem;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    margin-bottom: 0.8rem;
    font-size: 0.85rem;
  }
  .field span {
    color: var(--muted);
  }
  select,
  input[type='file'] {
    font: inherit;
    padding: 0.4rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: inherit;
  }
  .muted {
    color: var(--muted);
    font-size: 0.85rem;
  }
  .report {
    font-size: 0.95rem;
  }
  .row {
    display: flex;
    gap: 0.5rem;
    margin-top: 1rem;
  }
  .row.end {
    justify-content: flex-end;
  }
  button {
    font: inherit;
    padding: 0.4rem 0.9rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: inherit;
    cursor: pointer;
  }
  button.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: white;
  }
  button:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .bar {
    height: 0.4rem;
    background: var(--hover);
    border-radius: 999px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.2s;
  }
  .error {
    color: #c33;
    font-size: 0.85rem;
  }
</style>
