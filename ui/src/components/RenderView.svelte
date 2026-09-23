<script lang="ts">
  import { onDestroy, onMount } from 'svelte'
  import { api, type RenderFormat } from '../lib/api.ts'
  import { displayName, type VaultSession } from '../lib/vault.svelte.ts'
  import { beforeBodyEnd } from '../lib/render.ts'

  /**
   * A note rendered through Quarto (SPEC §5.6), in a pane beside it. Quarto's page is its own
   * document — its scripts, its styles — so it lives in a sandboxed frame with no access to
   * this origin: it can run the scripts that draw it, and cannot reach the app, its session or
   * its API. A render takes seconds, so it runs when asked; an edit since the last one only
   * marks it stale.
   *
   * It renders what the note asks for — the first format its front matter declares — unless
   * the picker says otherwise. A page (HTML, slides) shows here; a file (PDF, Word) downloads,
   * and the pane says so.
   */
  let {
    session,
    noteId,
    visible = true,
  }: {
    session: VaultSession
    noteId: string
    /** A tab behind another is not rendered until it is first brought forward. */
    visible?: boolean
  } = $props()

  let html: string | null = $state(null)
  let error: string | null = $state(null)
  let busy = $state(false)
  /** The note as it is now, and as it was when the page on screen was rendered. */
  let text: string | null = $state(null)
  let renderedFrom: string | null = $state(null)
  let stale = $derived(html !== null && text !== null && renderedFrom !== null && text !== renderedFrom)
  let stop: (() => void) | undefined

  const CHOICES: { id: RenderFormat | 'auto'; label: string }[] = [
    { id: 'auto', label: 'As the note says' },
    { id: 'html', label: 'Page (HTML)' },
    { id: 'revealjs', label: 'Slides (reveal.js)' },
    { id: 'pdf', label: 'PDF — download' },
    { id: 'docx', label: 'Word — download' },
  ]
  let choice: RenderFormat | 'auto' = $state('auto')
  /** What the last render turned out to be, for the bar to name: `auto` resolves on the server. */
  let made = $state('')
  /** A render that was a file rather than a page: what was saved. */
  let saved: { name: string; url: string } | null = $state(null)
  const KINDS: Record<string, string> = { 'text/html': 'page', 'application/pdf': 'PDF', 'application/vnd.openxmlformats-officedocument.wordprocessingml.document': 'Word file' }

  function save(blob: Blob, name: string) {
    if (saved) URL.revokeObjectURL(saved.url)
    saved = { name, url: URL.createObjectURL(blob) }
    const a = document.createElement('a')
    a.href = saved.url
    a.download = name
    a.click()
  }

  /**
   * Links leave the frame: one to another site opens a tab of its own rather than trying to
   * load inside the pane, where most sites refuse to be framed. A `#section` link stays, so
   * Quarto's table of contents still scrolls the page.
   */
  const LINKS_OUT = `<script>document.addEventListener('click',function(e){var a=e.target.closest&&e.target.closest('a[href]');if(a&&a.getAttribute('href').charAt(0)!=='#'){a.target='_blank';a.rel='noopener noreferrer'}},true)<\/script>`

  async function render() {
    if (busy) return
    busy = true
    const from = text
    try {
      const r = await api.render(session.id, noteId, choice)
      const type = (r.headers.get('content-type') ?? '').split(';')[0]!.trim()
      if (r.status === 501) {
        error = 'Rendering needs Quarto, and there is none here — or the server has it switched off.'
      } else if (!r.ok) {
        error = (await r.text()).trim() || `Rendering failed (${r.status}).`
      } else if (type === 'text/html') {
        const page = await r.text()
        html = beforeBodyEnd(page, LINKS_OUT)
        // A self-contained deck carries its scripts first; the markup that makes it one is late.
        made = page.includes('<div class="reveal') ? 'slides' : 'page'
        saved = null
        renderedFrom = from
        error = null
      } else {
        // A file, not a page: it goes to the downloads, and the pane keeps what it showed.
        const ext = r.headers.get('content-disposition')?.match(/filename="[^"]*\.([^".]+)"/u)?.[1] ?? 'bin'
        save(await r.blob(), `${displayName(session.pathOf(noteId) ?? 'note')}.${ext}`)
        made = KINDS[type] ?? 'file'
        renderedFrom = from
        error = null
      }
    } catch (e) {
      error = `Rendering failed: ${String(e)}`
    } finally {
      busy = false
    }
  }

  onMount(() => {
    stop = session.watchNote(noteId, (t) => (text = t))
  })
  // The first render waits for the note, so that "stale" has something to compare with, and for
  // the tab to be the one showing.
  let started = false
  $effect(() => {
    if (!visible || text === null || started) return
    started = true
    void render()
  })
  onDestroy(() => {
    stop?.()
    if (saved) URL.revokeObjectURL(saved.url)
  })
</script>

<div class="render">
  <div class="bar">
    <select bind:value={choice} onchange={render} disabled={busy} aria-label="Render as">
      {#each CHOICES as c (c.id)}<option value={c.id}>{c.label}</option>{/each}
    </select>
    <span class="state">
      {#if busy}
        Rendering with Quarto…
      {:else if error}
        Not rendered
      {:else if stale}
        Changed since this render
      {:else if made}
        Rendered as {made === 'slides' ? 'slides' : `a ${made}`}
      {/if}
    </span>
    <span class="spacer"></span>
    <button onclick={render} disabled={busy} title="Render the note again">{html === null && !saved ? 'Render' : 'Re-render'}</button>
  </div>
  {#if error}
    <pre class="error">{error}</pre>
  {/if}
  {#if saved}
    <div class="saved">
      <p>Saved <strong>{saved.name}</strong> to your downloads.</p>
      <p class="muted">
        <a href={saved.url} download={saved.name}>Save it again</a> · or pick <em>Page</em> or <em>Slides</em> above to see
        the note here.
      </p>
    </div>
  {/if}
  {#if html !== null && !saved}
    <iframe class:dim={busy} title="Rendered note" sandbox="allow-scripts allow-popups allow-popups-to-escape-sandbox" srcdoc={html}></iframe>
  {:else if busy}
    <p class="wait">Quarto takes a few seconds to start.</p>
  {/if}
</div>

<style>
  .render {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.35rem 0.75rem;
    border-bottom: 1px solid var(--border-soft);
    font-family: var(--ui);
    font-size: 0.75rem;
    color: var(--muted);
  }
  .spacer {
    flex: 1;
  }
  .bar button {
    font: inherit;
    color: var(--fg);
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 0.15rem 0.6rem;
    cursor: pointer;
  }
  .bar button:hover:not(:disabled) {
    background: var(--hover);
  }
  .bar button:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .error {
    margin: 0;
    padding: 0.75rem;
    font-family: var(--mono);
    font-size: 0.75rem;
    white-space: pre-wrap;
    color: var(--danger);
    border-bottom: 1px solid var(--border-soft);
  }
  .bar select {
    font: inherit;
    color: var(--fg);
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 5px;
    padding: 0.1rem 0.3rem;
  }
  .saved {
    padding: 1.2rem 1rem;
    font-family: var(--ui);
    font-size: 0.85rem;
  }
  .saved p {
    margin: 0 0 0.4rem;
  }
  .saved .muted {
    color: var(--muted);
  }
  .saved a {
    color: var(--accent);
  }
  .wait {
    margin: 0;
    padding: 1rem;
    font-family: var(--ui);
    font-size: 0.8rem;
    color: var(--faint);
  }
  /* Quarto's page brings its own background; white is what it was designed on. */
  iframe {
    flex: 1;
    min-height: 0;
    width: 100%;
    border: 0;
    background: white;
    transition: opacity 0.2s;
  }
  iframe.dim {
    opacity: 0.55;
  }
</style>
