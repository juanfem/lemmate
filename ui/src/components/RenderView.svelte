<script lang="ts">
  import { onDestroy, onMount } from 'svelte'
  import { api } from '../lib/api.ts'
  import type { VaultSession } from '../lib/vault.svelte.ts'

  /**
   * A note rendered through Quarto (SPEC §5.6), in a pane beside it. Quarto's page is its own
   * document — its scripts, its styles — so it lives in a sandboxed frame with no access to
   * this origin: it can run the scripts that draw it, and cannot reach the app, its session or
   * its API. A render takes seconds, so it runs when asked; an edit since the last one only
   * marks it stale.
   */
  let { session, noteId }: { session: VaultSession; noteId: string } = $props()

  let html: string | null = $state(null)
  let error: string | null = $state(null)
  let busy = $state(false)
  /** The note as it is now, and as it was when the page on screen was rendered. */
  let text: string | null = $state(null)
  let renderedFrom: string | null = $state(null)
  let stale = $derived(html !== null && text !== null && renderedFrom !== null && text !== renderedFrom)
  let stop: (() => void) | undefined

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
      const r = await api.render(session.id, noteId, 'preview')
      if (r.status === 501) {
        error = 'Rendering needs Quarto, and there is none here — or the server has it switched off.'
      } else if (!r.ok) {
        error = (await r.text()).trim() || `Rendering failed (${r.status}).`
      } else {
        const page = await r.text()
        html = page.includes('</body>') ? page.replace('</body>', `${LINKS_OUT}</body>`) : page + LINKS_OUT
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
    stop = session.watchNote(noteId, (t) => {
      const first = text === null
      text = t
      // The first render waits for the note, so that "stale" has something to compare with.
      if (first) void render()
    })
  })
  onDestroy(() => stop?.())
</script>

<div class="render">
  <div class="bar">
    <span class="state">
      {#if busy}
        Rendering with Quarto…
      {:else if error}
        Not rendered
      {:else if stale}
        Changed since this render
      {:else if html !== null}
        Rendered with Quarto
      {/if}
    </span>
    <span class="spacer"></span>
    <button onclick={render} disabled={busy} title="Render the note again">{html === null ? 'Render' : 'Re-render'}</button>
  </div>
  {#if error}
    <pre class="error">{error}</pre>
  {/if}
  {#if html !== null}
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
