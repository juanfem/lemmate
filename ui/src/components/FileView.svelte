<script lang="ts">
  import { onDestroy, onMount, untrack } from 'svelte'
  import type { EditorView } from '@codemirror/view'
  import { api, type FileEntry } from '../lib/api.ts'
  import { createFileEditor } from '../lib/editor/setup.ts'
  import { baseName, fileKind, folderOf, humanSize, isText } from '../lib/filetree.ts'
  import { displayName, type VaultSession } from '../lib/vault.svelte.ts'

  /**
   * A vault file that is not a note, in a tab of its own (SPEC §9). Text — a stylesheet,
   * `_quarto.yml` — opens in an editor; anything else shows what it can and offers the bytes.
   *
   * A file is not a CRDT the way a note is: two people editing one would each save a whole
   * file. So a save names the version it started from, and when the file changed in between
   * the save stops and asks rather than overwriting somebody's work.
   */
  let {
    session,
    path,
    onOpenNote,
    onRename,
    onDelete,
  }: {
    session: VaultSession
    path: string
    onOpenNote: (id: string) => void
    onRename: (entry: FileEntry) => void
    onDelete: (entry: FileEntry) => void
  } = $props()

  onMount(() => session.watchFiles())

  let entry = $derived(session.files.find((f) => f.path === path))
  let text = $derived(isText(path))
  let url = $derived(entry ? api.attachmentUrl(session.id, entry.hash) : '')

  // ---- text
  let host: HTMLDivElement | undefined = $state()
  let view: EditorView | undefined
  /** The hash of the version the editor started from, and its text. */
  let base: string | null = $state(null)
  let baseText = ''
  let dirty = $state(false)
  let busy = $state(false)
  let error = $state('')
  /** Someone else saved since `base`: the hash they left. */
  let conflict: string | null = $state(null)

  async function fetchText(hash: string): Promise<string> {
    const r = await fetch(api.attachmentUrl(session.id, hash))
    if (!r.ok) throw new Error(`${r.status}`)
    return r.text()
  }

  function mount(content: string, hash: string) {
    view?.destroy()
    base = hash
    baseText = content
    dirty = false
    if (!host) return
    view = createFileEditor(host, content, baseName(path), {
      onChange: (t) => (dirty = t !== baseText),
      save: () => void save(),
    })
  }

  // Load once the listing names the file; afterwards, follow a change made elsewhere only
  // while there is nothing unsaved here to lose.
  $effect(() => {
    const e = entry
    if (!text || !e || !host) return
    untrack(() => {
      if (e.hash === base) return
      if (base !== null && dirty) {
        conflict = e.hash
        return
      }
      fetchText(e.hash).then(
        (t) => mount(t, e.hash),
        (err) => (error = `Could not load the file (${String(err)}).`),
      )
    })
  })

  async function save(overwrite = false) {
    if (!view || busy || (!dirty && !overwrite)) return
    busy = true
    error = ''
    const content = view.state.doc.toString()
    try {
      const r = await api.putFile(session.id, path, new TextEncoder().encode(content), {
        replace: true,
        base: overwrite && conflict ? conflict : (base ?? undefined),
      })
      if (r.ok) {
        base = r.hash
        baseText = content
        dirty = false
        conflict = null
      } else {
        conflict = r.conflict
      }
    } catch (e) {
      error = `Not saved: ${String(e)}`
    } finally {
      busy = false
    }
  }

  async function discard() {
    const hash = conflict ?? entry?.hash
    if (!hash) return
    conflict = null
    mount(await fetchText(hash), hash)
  }

  // ---- anything else
  let picker: HTMLInputElement | undefined = $state()
  async function replaceWith(files: FileList | null) {
    const file = files?.[0]
    if (!file || !entry) return
    busy = true
    error = ''
    try {
      const r = await api.putFile(session.id, path, new Uint8Array(await file.arrayBuffer()), { replace: true, base: entry.hash })
      if (!r.ok) error = 'The file changed while you were choosing; look again, then replace it.'
    } catch (e) {
      error = `Not replaced: ${String(e)}`
    } finally {
      busy = false
      if (picker) picker.value = ''
    }
  }

  function download() {
    if (!entry) return
    const a = document.createElement('a')
    a.href = url
    a.download = baseName(path)
    a.click()
  }

  function beforeUnload(e: BeforeUnloadEvent) {
    if (dirty) e.preventDefault()
  }
  onMount(() => window.addEventListener('beforeunload', beforeUnload))
  onDestroy(() => {
    window.removeEventListener('beforeunload', beforeUnload)
    view?.destroy()
  })

  let users = $derived((entry?.used_by ?? []).map((id) => ({ id, label: displayName(session.pathOf(id) ?? id) })))
</script>

<div class="file">
  <header>
    <div class="where">
      {#if folderOf(path)}<span class="dir">{folderOf(path).replaceAll('/', ' / ')} /</span>{/if}
      <strong>{baseName(path)}</strong>
      {#if entry}
        <span class="dim">· {humanSize(entry.size)}</span>
        {#if users.length}
          <span class="dim">· used by</span>
          {#each users as u, i (u.id)}<button class="link" onclick={() => onOpenNote(u.id)}>{u.label}</button>{#if i < users.length - 1}<span class="dim">,</span>{/if}{/each}
        {:else if entry.vault}
          <span class="dim">· for the whole vault</span>
        {:else}
          <span class="dim">· not used by any note</span>
        {/if}
      {/if}
    </div>
    <div class="actions">
      {#if text && dirty}<span class="dim">Unsaved changes</span>{/if}
      {#if entry}
        <button onclick={() => onRename(entry!)} disabled={busy}>Rename / move…</button>
        <button class="danger" onclick={() => onDelete(entry!)} disabled={busy}>Delete…</button>
        {#if !text}<button onclick={() => picker?.click()} disabled={busy}>Replace…</button>{/if}
        <button onclick={download}>Download</button>
        {#if text}<button class="primary" onclick={() => save()} disabled={busy || !dirty} title="Save (Ctrl+S)">Save</button>{/if}
      {/if}
      <input type="file" bind:this={picker} hidden onchange={(e) => replaceWith(e.currentTarget.files)} />
    </div>
  </header>

  {#if conflict}
    <div class="banner warn" role="alert">
      <span>This file was changed elsewhere since you opened it.</span>
      <button onclick={() => save(true)} disabled={busy}>Save mine over it</button>
      <button onclick={discard} disabled={busy}>Discard mine, load theirs</button>
    </div>
  {/if}
  {#if error}<div class="banner err" role="alert">{error}</div>{/if}

  {#if !entry}
    <p class="empty">{session.files.length ? 'This file is not in the vault any more.' : 'Loading…'}</p>
  {:else if text}
    <div class="editor" bind:this={host}></div>
    <p class="note">Files are not edited live like notes: Save replaces the file for everyone, and the last save wins.</p>
  {:else if fileKind(path) === 'image'}
    <div class="preview"><img src={url} alt={baseName(path)} /></div>
  {:else}
    <div class="preview none">
      <p>No preview for this kind of file.</p>
      <button onclick={download}>Download it</button>
    </div>
  {/if}
</div>

<style>
  .file {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    font-family: var(--ui);
  }
  header {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-wrap: wrap;
    padding: 0.55rem 1rem;
    border-bottom: 1px solid var(--border-soft);
    font-size: 0.8rem;
  }
  .where {
    display: flex;
    align-items: baseline;
    gap: 0.3rem;
    flex-wrap: wrap;
    flex: 1;
    min-width: 0;
  }
  .dir,
  .dim {
    color: var(--muted);
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--accent);
    cursor: pointer;
  }
  .link:hover {
    text-decoration: underline;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    flex-wrap: wrap;
  }
  .actions button,
  .banner button,
  .none button {
    font: inherit;
    font-size: 0.78rem;
    padding: 0.25rem 0.65rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg);
    color: var(--fg);
    cursor: pointer;
  }
  .actions button:hover:not(:disabled) {
    background: var(--hover);
  }
  .actions button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .actions .primary {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--accent-fg);
    font-weight: 600;
  }
  .actions .primary:hover:not(:disabled) {
    background: var(--accent);
    filter: brightness(1.08);
  }
  .actions .danger {
    color: var(--danger);
  }
  .banner {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
    padding: 0.5rem 1rem;
    font-size: 0.8rem;
  }
  .banner.warn {
    background: color-mix(in srgb, var(--accent-bg) 70%, transparent);
  }
  .banner.err {
    background: var(--danger-bg);
    color: var(--danger);
  }
  .editor {
    flex: 1;
    min-height: 0;
    overflow: hidden;
  }
  .note {
    margin: 0;
    padding: 0.5rem 1rem;
    border-top: 1px solid var(--border-soft);
    font-size: 0.72rem;
    color: var(--muted);
  }
  .empty {
    padding: 1.5rem;
    color: var(--muted);
  }
  .preview {
    flex: 1;
    min-height: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 1.5rem;
    background-color: var(--code-bg);
    background-image: linear-gradient(45deg, var(--hover) 25%, transparent 25%, transparent 75%, var(--hover) 75%),
      linear-gradient(45deg, var(--hover) 25%, transparent 25%, transparent 75%, var(--hover) 75%);
    background-size: 20px 20px;
    background-position:
      0 0,
      10px 10px;
    overflow: auto;
  }
  .preview img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
    box-shadow: 0 2px 10px rgb(0 0 0 / 0.12);
  }
  .preview.none {
    flex-direction: column;
    gap: 0.6rem;
    background: none;
    color: var(--muted);
  }
</style>
