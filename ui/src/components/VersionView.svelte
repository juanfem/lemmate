<script lang="ts">
  import { onDestroy, onMount } from 'svelte'
  import * as Y from 'yjs'
  import { Awareness } from 'y-protocols/awareness'
  import { Decoration, EditorView, type DecorationSet } from '@codemirror/view'
  import { EditorState, StateEffect, StateField } from '@codemirror/state'
  import { createEditor } from '../lib/editor/setup.ts'
  import { openWebLink } from '../lib/linktarget.ts'
  import { changedLines } from '../lib/diff.ts'

  /**
   * One past version of a note, rendered the way the note itself is rendered — same markdown
   * pipeline, same measure, same paper. A second renderer would be a second thing to keep in
   * step with the first, so this is the real editor in reading mode over a throwaway document.
   */
  let {
    content,
    current,
    embedUrl,
  }: {
    content: string
    current: string
    /** Embeds still resolve against the vault as it is now: an attachment is not versioned. */
    embedUrl: (target: string) => string | undefined
  } = $props()

  let host: HTMLDivElement
  let view: EditorView | undefined
  let doc: Y.Doc | undefined

  /** Lines this version has that the note no longer does. The version never changes; the note
   *  it is compared with may, so the marks are replaced through this effect when it does. */
  const setMarks = StateEffect.define<DecorationSet>()
  function marks(text: string, now: string): DecorationSet {
    const changed = changedLines(text, now)
    if (changed.size === 0) return Decoration.none
    const lines = text.split('\n')
    const ranges = []
    let at = 0
    for (let i = 0; i < lines.length; i++) {
      if (changed.has(i)) ranges.push(Decoration.line({ class: 'cm-changed' }).range(at))
      at += lines[i]!.length + 1
    }
    return Decoration.set(ranges)
  }

  onMount(() => {
    doc = new Y.Doc()
    const text = doc.getText('content')
    text.insert(0, content)
    const deco = marks(content, current)
    view = createEditor(host, text, new Awareness(doc), {
      mode: 'reading',
      embedUrl,
      openUrl: openWebLink,
      openLink: () => {
        /* a link in an old version points at the note as it is now, not as it was */
      },
      extra: [
        StateField.define<DecorationSet>({
          create: () => deco,
          update: (d, tr) => tr.effects.find((e) => e.is(setMarks))?.value ?? d,
          provide: (f) => EditorView.decorations.from(f),
        }),
        // A throwaway document: a click on a task box must not tick it in here.
        EditorState.readOnly.of(true),
      ],
    })
  })

  $effect(() => {
    const now = current
    if (view) view.dispatch({ effects: setMarks.of(marks(content, now)) })
  })

  onDestroy(() => {
    view?.destroy()
    doc?.destroy()
  })
</script>

<div class="version" bind:this={host}></div>

<style>
  .version {
    height: 100%;
    min-height: 0;
    overflow: auto;
  }
</style>
