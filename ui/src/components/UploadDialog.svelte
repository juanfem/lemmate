<script lang="ts">
  import { untrack } from 'svelte'
  import { api } from '../lib/api.ts'
  import type { VaultSession } from '../lib/vault.svelte.ts'
  import { displayName } from '../lib/vault.svelte.ts'
  import { folderOf, freeName, humanSize } from '../lib/filetree.ts'

  /**
   * Putting files in a vault on purpose (SPEC §9) — as opposed to pasting one into a note,
   * which files it under `attachments/` and links it. Here the person says where each goes:
   * next to the note they are working on, in `attachments/`, or in a folder of their choosing;
   * and when a name is taken, whether to replace that file or keep both.
   */
  let {
    session,
    files,
    folder,
    note,
    vaultLabel,
    onDone,
    onClose,
  }: {
    session: VaultSession
    files: File[]
    /** Where the person asked to upload — a folder's "upload here" — or `null` to choose. */
    folder: string | null
    /** The note open in this vault, if any: "next to it" is then the first choice. */
    note: { id: string; path: string } | null
    /** Named in the title when there is more than one vault to be in. */
    vaultLabel?: string
    onDone: (paths: string[]) => void
    onClose: () => void
  } = $props()

  const noteDir = untrack(() => (note ? folderOf(note.path) : null))
  type Where = 'note' | 'attachments' | 'folder'
  let where: Where = $state(
    untrack(() => (folder !== null ? (folder === noteDir ? 'note' : folder === 'attachments' ? 'attachments' : 'folder') : note ? 'note' : 'attachments')),
  )
  let other = $state(untrack(() => folder ?? ''))
  let target = $derived(
    (where === 'note' ? (noteDir ?? '') : where === 'attachments' ? 'attachments' : other.trim().replace(/^\/+|\/+$/gu, '')),
  )

  /** Every folder the vault has, notes' and files' alike, to complete "a folder" from. */
  let folders = $derived.by(() => {
    const out = new Set<string>()
    for (const p of [...session.notes.map((n) => n.path), ...session.files.map((f) => f.path)]) {
      const parts = p.split('/').slice(0, -1)
      for (let i = 1; i <= parts.length; i++) out.add(parts.slice(0, i).join('/'))
    }
    return [...out].sort()
  })

  const at = (dir: string, name: string) => (dir ? `${dir}/${name}` : name)
  const taken = (p: string) => session.files.some((f) => f.path === p)
  /** Per file, when its name is taken: replace that file, or keep both. */
  let choices: Record<string, 'replace' | 'keep'> = $state({})
  let plan = $derived(
    files.map((file) => {
      const path = at(target, file.name)
      const existing = session.files.find((f) => f.path === path)
      const choice = choices[file.name] ?? 'replace'
      return {
        file,
        path,
        existing,
        choice,
        final: existing && choice === 'keep' ? freeName(taken, target, file.name) : path,
      }
    }),
  )
  let badTarget = $derived(/(^|\/)\.|\.\.|\\/u.test(target))

  let busy = $state(false)
  let error = $state('')
  async function upload() {
    busy = true
    error = ''
    const written: string[] = []
    const failed: string[] = []
    for (const p of plan) {
      try {
        const bytes = new Uint8Array(await p.file.arrayBuffer())
        const replace = !!p.existing && p.choice === 'replace'
        const r = await api.putFile(session.id, p.final, bytes, { replace, base: replace ? p.existing!.hash : undefined })
        if (r.ok) written.push(r.path)
        else failed.push(`${p.final} changed meanwhile`)
      } catch (e) {
        failed.push(`${p.final}: ${e instanceof Error ? e.message : String(e)}`)
      }
    }
    busy = false
    if (failed.length) error = `Not uploaded — ${failed.join('; ')}.`
    else onDone(written)
  }
  let usersOf = (ids: string[]) => ids.map((id) => displayName(session.pathOf(id) ?? '')).filter(Boolean)
</script>

<div class="backdrop" onmousedown={() => !busy && onClose()} role="presentation">
  <div class="dialog" onmousedown={(e) => e.stopPropagation()} role="dialog" aria-label="Upload files" tabindex="-1">
    <h2>Upload {files.length === 1 ? files[0]!.name : `${files.length} files`}{#if vaultLabel}<span class="muted"> to {vaultLabel}</span>{/if}</h2>

    <fieldset class="where" disabled={busy}>
      <legend>Put {files.length === 1 ? 'it' : 'them'}</legend>
      {#if note}
        <label class:on={where === 'note'}>
          <input type="radio" bind:group={where} value="note" />
          <span><strong>Next to {displayName(note.path)}</strong><small>{noteDir ? `${noteDir}/` : 'the vault root'}</small></span>
        </label>
      {/if}
      <label class:on={where === 'attachments'}>
        <input type="radio" bind:group={where} value="attachments" />
        <span><strong>In attachments</strong><small>attachments/</small></span>
      </label>
      <label class:on={where === 'folder'}>
        <input type="radio" bind:group={where} value="folder" />
        <span><strong>In a folder</strong>
          <input
            class="folder"
            list="upload-folders"
            placeholder="e.g. Slides/2026 Review"
            bind:value={other}
            onfocus={() => (where = 'folder')}
            aria-label="Folder"
          />
        </span>
      </label>
      <datalist id="upload-folders">{#each folders as f (f)}<option value={f}></option>{/each}</datalist>
    </fieldset>

    <ul class="files">
      {#each plan as p (p.file.name)}
        <li>
          <div class="line">
            <!-- The path asked for; when it clashes, the choice under it says where the copy goes. -->
            <span class="path"><span class="muted">{p.path.includes('/') ? `${folderOf(p.path)}/` : ''}</span>{p.path.slice(p.path.lastIndexOf('/') + 1)}</span>
            {#if p.existing}<span class="tag">Already there</span>{:else}<span class="muted small">New · {humanSize(p.file.size)}</span>{/if}
          </div>
          {#if p.existing}
            <div class="clash">
              <label><input type="radio" name="c-{p.file.name}" checked={p.choice === 'replace'} onchange={() => (choices[p.file.name] = 'replace')} disabled={busy} /> Replace it</label>
              {#if usersOf(p.existing.used_by).length}<span class="muted small">— {usersOf(p.existing.used_by).join(', ')} {p.existing.used_by.length === 1 ? 'uses' : 'use'} it</span>{/if}
              <span class="gap"></span>
              <label><input type="radio" name="c-{p.file.name}" checked={p.choice === 'keep'} onchange={() => (choices[p.file.name] = 'keep')} disabled={busy} /> Keep both, as {freeName(taken, target, p.file.name).split('/').pop()}</label>
            </div>
          {/if}
        </li>
      {/each}
    </ul>

    {#if badTarget}<p class="error">That folder name is not one a vault can have.</p>{/if}
    {#if error}<p class="error">{error}</p>{/if}
    <div class="row end">
      <button onclick={onClose} disabled={busy}>Cancel</button>
      <button class="primary" onclick={upload} disabled={busy || badTarget || files.length === 0}>
        {busy ? 'Uploading…' : files.length === 1 ? 'Upload' : `Upload ${files.length} files`}
      </button>
    </div>
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
    overflow: auto;
  }
  .dialog {
    width: min(38rem, 92vw);
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: 0 10px 40px rgb(0 0 0 / 0.3);
    padding: 1rem 1.2rem 1.2rem;
  }
  h2 {
    margin: 0 0 0.8rem;
    font-size: 1.1rem;
    overflow-wrap: anywhere;
  }
  .muted {
    color: var(--muted);
  }
  .small {
    font-size: 0.8rem;
  }
  .where {
    border: 0;
    margin: 0 0 0.9rem;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
    gap: 0.5rem;
  }
  .where legend {
    padding: 0 0 0.4rem;
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--muted);
  }
  .where label {
    display: flex;
    align-items: flex-start;
    gap: 0.45rem;
    padding: 0.55rem 0.65rem;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    cursor: pointer;
    font-size: 0.85rem;
  }
  .where label.on {
    border-color: var(--accent);
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .where label > span {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
    flex: 1;
  }
  .where small {
    color: var(--muted);
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }
  .where input[type='radio'] {
    margin: 0.15rem 0 0;
    accent-color: var(--accent);
  }
  .folder {
    font: inherit;
    font-size: 0.8rem;
    padding: 0.2rem 0.35rem;
    border: 1px solid var(--border);
    border-radius: 5px;
    background: var(--panel);
    color: inherit;
    width: 100%;
    box-sizing: border-box;
  }
  .files {
    list-style: none;
    margin: 0;
    padding: 0;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg);
    max-height: 16rem;
    overflow: auto;
    font-size: 0.85rem;
  }
  .files li {
    padding: 0.5rem 0.7rem;
  }
  .files li + li {
    border-top: 1px solid var(--border-soft);
  }
  .line {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }
  .path {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .tag {
    font-size: 0.75rem;
    padding: 0.05rem 0.5rem;
    border-radius: 999px;
    background: color-mix(in srgb, var(--danger-bg) 60%, var(--accent-bg));
    color: var(--fg);
    white-space: nowrap;
  }
  .clash {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
    padding: 0.35rem 0 0 0.2rem;
    font-size: 0.8rem;
  }
  .clash input {
    accent-color: var(--accent);
  }
  .gap {
    flex: 1;
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
    color: var(--accent-fg);
  }
  button:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .error {
    color: var(--danger);
    font-size: 0.85rem;
  }
  @media (max-width: 720px) {
    .backdrop {
      padding-top: 5vh;
    }
  }
</style>
