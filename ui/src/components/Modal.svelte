<script lang="ts">
  import { untrack } from 'svelte'

  let {
    title,
    body = '',
    kind,
    initial = '',
    placeholder = '',
    confirmLabel = 'OK',
    danger = false,
    onSubmit,
    onCancel,
  }: {
    title: string
    /** Optional detail under the title; blank lines and newlines are kept. */
    body?: string
    kind: 'prompt' | 'confirm'
    initial?: string
    placeholder?: string
    confirmLabel?: string
    danger?: boolean
    onSubmit: (value: string) => void
    onCancel: () => void
  } = $props()

  // `initial` seeds the field once; later edits live in `value`.
  let value = $state(untrack(() => initial))
  // The input only exists for `kind === 'prompt'`, so the ref has to be reactive.
  let input = $state<HTMLInputElement | undefined>()
  let confirmButton: HTMLButtonElement

  function submit() {
    onSubmit(kind === 'prompt' ? value : '')
  }

  // Native dialogs are unavailable in the Tauri webview, so the modal owns the
  // keyboard while it is open (App.onKey bails out for the same reason).
  function onKey(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      // preventDefault also stops the focused button's implicit click.
      e.preventDefault()
      submit()
    } else if (e.key === 'Escape') {
      e.preventDefault()
      onCancel()
    }
  }

  $effect(() => {
    if (kind === 'prompt') {
      input?.focus()
      input?.select()
    } else confirmButton?.focus()
  })
</script>

<svelte:window onkeydown={onKey} />

<div class="backdrop" onmousedown={onCancel} role="presentation">
  <div class="dialog" onmousedown={(e) => e.stopPropagation()} role="dialog" aria-modal="true" aria-label={title} tabindex="-1">
    <h2>{title}</h2>
    {#if body}<p class="body">{body}</p>{/if}
    {#if kind === 'prompt'}
      <input bind:this={input} bind:value {placeholder} />
    {/if}
    <div class="actions">
      <button onclick={onCancel}>Cancel</button>
      <button class="confirm" class:danger bind:this={confirmButton} onclick={submit}>{confirmLabel}</button>
    </div>
  </div>
</div>

<style>
  .body {
    margin: 0 0 0.9rem;
    font-size: 0.85rem;
    line-height: 1.45;
    color: var(--muted);
    white-space: pre-wrap;
    max-height: 12rem;
    overflow: auto;
  }
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 0.3);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 18vh;
    z-index: 20;
    overflow: auto;
  }
  .dialog {
    width: min(32rem, 90vw);
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: 0 10px 40px rgb(0 0 0 / 0.3);
    overflow: hidden;
    outline: none;
  }
  h2 {
    margin: 0;
    padding: 0.9rem 1rem;
    font-size: 1rem;
    font-weight: 600;
    border-bottom: 1px solid var(--border);
  }
  input {
    width: 100%;
    font: inherit;
    font-size: 1rem;
    padding: 0.7rem 1rem;
    border: 0;
    border-bottom: 1px solid var(--border);
    background: transparent;
    color: inherit;
    outline: none;
  }
  input:focus {
    background: var(--accent-bg);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    padding: 0.7rem 1rem;
  }
  .actions button {
    font: inherit;
    font-size: 0.9rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: none;
    color: var(--muted);
    padding: 0.35rem 0.9rem;
    cursor: pointer;
  }
  .actions button:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .actions button.confirm {
    border-color: transparent;
    background: var(--accent);
    color: white;
  }
  .actions button.confirm.danger {
    background: #dc2626;
  }
  .actions button.confirm:hover {
    filter: brightness(1.1);
    color: white;
  }
  .actions button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  /* A phone has no room to spare above an overlay, and a long one must be able to scroll. */
  @media (max-width: 720px) {
    .backdrop {
      padding-top: 8vh;
    }
  }
</style>
