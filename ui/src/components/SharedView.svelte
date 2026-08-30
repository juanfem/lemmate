<script lang="ts">
  import { api } from '../lib/api.ts'
  import { onMount } from 'svelte'
  import { createEditor } from '../lib/editor/setup.ts'
  import { EditorView } from '@codemirror/view'
  import * as Y from 'yjs'
  import { Awareness } from 'y-protocols/awareness'

  let { token }: { token: string } = $props()
  let title = $state('')
  let error = $state('')
  let host: HTMLDivElement

  onMount(async () => {
    try {
      const note = await api.publicNote(token)
      title = note.title ?? note.path
      document.title = `${title} · notes`
      // A throwaway local doc rendered with the same live preview, read-only.
      const doc = new Y.Doc()
      doc.getText('content').insert(0, note.content)
      createEditor(host, doc.getText('content'), new Awareness(doc), {
        openLink: () => {},
        embedUrl: () => undefined,
        extra: [EditorView.editable.of(false)],
      })
    } catch {
      error = 'This link is not valid (it may have been revoked).'
    }
  })
</script>

<main class="shared">
  <header><strong>{title || 'Shared note'}</strong><span class="muted">read-only · shared with notes</span></header>
  {#if error}<p class="error">{error}</p>{/if}
  <div class="editor" bind:this={host}></div>
</main>

<style>
  .shared { height: 100%; display: grid; grid-template-rows: auto 1fr; }
  header { display: flex; justify-content: space-between; padding: 0.5rem 1rem; border-bottom: 1px solid var(--border); background: var(--panel); font-size: 0.9rem; }
  .muted { color: var(--muted); }
  .editor { overflow: auto; }
  .error { padding: 2rem; color: #dc2626; }
</style>
