// The full-width preference as a rune, for the menus that show it ticked. `lib/measure.ts`
// holds the storage and the root attribute; this only keeps them and the UI in step, across
// windows too — another window's toggle arrives as a `storage` event.
import { MEASURE_KEY, applyFullWidth, readFullWidth, writeFullWidth } from './measure.ts'

const storage = typeof localStorage === 'undefined' ? undefined : localStorage
const root = typeof document === 'undefined' ? undefined : document.documentElement

let full = $state(readFullWidth(storage))
applyFullWidth(root, full)

if (typeof window !== 'undefined') {
  window.addEventListener('storage', (e) => {
    if (e.key !== MEASURE_KEY && e.key !== null) return
    full = readFullWidth(storage)
    applyFullWidth(root, full)
  })
}

export const measure = {
  get full() {
    return full
  },
  toggle() {
    full = !full
    writeFullWidth(storage, full)
    applyFullWidth(root, full)
  },
}
