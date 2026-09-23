// What the render pane does to Quarto's page before showing it (SPEC §5.6).

/**
 * `page` with `script` added just before its closing `</body>` — the *last* one. A reveal.js
 * deck carries another inside its speaker-notes plugin, a string of JavaScript that writes the
 * speaker window; putting the script there cut that string open and spilled the plugin onto
 * the slides as text.
 */
export function beforeBodyEnd(page: string, script: string): string {
  const at = page.lastIndexOf('</body>')
  return at === -1 ? page + script : page.slice(0, at) + script + page.slice(at)
}
