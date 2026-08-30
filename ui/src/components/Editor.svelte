<script lang="ts">
  import { onDestroy, onMount } from 'svelte'
  import { EditorView } from '@codemirror/view'
  import { createEditor } from '../lib/editor/setup.ts'
  import type { VaultSession } from '../lib/vault.svelte.ts'
  import { api } from '../lib/api.ts'
  import { displayName } from '../lib/vault.svelte.ts'

  let { session, noteId, onOpen }: { session: VaultSession; noteId: string; onOpen: (id: string) => void } = $props()

  let host: HTMLDivElement
  let view: EditorView | undefined
  let release: (() => void) | undefined

  function embedUrl(target: string): string | undefined {
    const t = target.trim()
    const path = session.pathOf(noteId) ?? ''
    const dir = path.includes('/') ? path.slice(0, path.lastIndexOf('/') + 1) : ''
    const name = t.split('/').pop() ?? t
    for (const candidate of [dir + t, t, `attachments/${name}`]) {
      const hash = session.attachments[candidate]
      if (hash) return api.attachmentUrl(session.id, hash)
    }
    const byName = Object.entries(session.attachments).find(([p]) => p.split('/').pop() === name)
    return byName ? api.attachmentUrl(session.id, byName[1]) : undefined
  }

  function openLink(target: string) {
    const hit = session.resolveLink(target)
    if (hit) onOpen(hit.id)
    else {
      const path = target.endsWith('.md') ? target : `${target}.md`
      onOpen(session.createNote(path, `# ${displayName(path)}\n\n`))
    }
  }

  /** Paste/drop files: upload, then reference them at the cursor (images as embeds). */
  async function insertFiles(files: FileList | File[], at: number) {
    if (!view) return
    const refs: string[] = []
    for (const file of Array.from(files)) {
      try {
        const path = await session.uploadAttachment(file.name, new Uint8Array(await file.arrayBuffer()), file.type)
        const name = path.split('/').pop() ?? path
        refs.push(file.type.startsWith('image/') ? `![[${name}]]` : `[${name}](${path})`)
      } catch (e) {
        refs.push(`<!-- upload failed for ${file.name}: ${String(e)} -->`)
      }
    }
    if (refs.length) view.dispatch({ changes: { from: at, insert: refs.join('\n') }, selection: { anchor: at + refs.join('\n').length } })
  }

  const fileHandlers = EditorView.domEventHandlers({
    paste(event, v) {
      const files = event.clipboardData?.files
      if (!files || files.length === 0) return false
      event.preventDefault()
      void insertFiles(files, v.state.selection.main.head)
      return true
    },
    drop(event, v) {
      const files = event.dataTransfer?.files
      if (!files || files.length === 0) return false
      event.preventDefault()
      const pos = v.posAtCoords({ x: event.clientX, y: event.clientY }) ?? v.state.selection.main.head
      void insertFiles(files, pos)
      return true
    },
  })

  onMount(() => {
    const acquired = session.acquire(noteId)
    release = acquired.release
    acquired.awareness.setLocalStateField('user', { name: localStorage.getItem('notes.user') ?? 'me', color: '#4c8bf5', colorLight: '#4c8bf533' })
    view = createEditor(host, acquired.doc.getText('content'), acquired.awareness, { openLink, embedUrl, extra: [fileHandlers] })
    view.focus()
  })

  onDestroy(() => {
    view?.destroy()
    release?.()
  })
</script>

<div class="editor" bind:this={host}></div>

<style>
  .editor {
    height: 100%;
    overflow: auto;
  }
</style>
