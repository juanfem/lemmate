// The palette as a rune, for the commands that name the other one. `lib/palette.ts` holds the
// storage and the root attribute; this only keeps them and the UI in step, across windows too —
// another window's choice arrives as a `storage` event.
import { PALETTE_KEY, applyPalette, readPalette, writePalette, type Palette } from './palette.ts'

const storage = typeof localStorage === 'undefined' ? undefined : localStorage
const root = typeof document === 'undefined' ? undefined : document.documentElement

let current = $state(readPalette(storage))
applyPalette(root, current)

if (typeof window !== 'undefined') {
  window.addEventListener('storage', (e) => {
    if (e.key !== PALETTE_KEY && e.key !== null) return
    current = readPalette(storage)
    applyPalette(root, current)
  })
}

export const colours = {
  get current() {
    return current
  },
  set(p: Palette) {
    current = p
    writePalette(storage, p)
    applyPalette(root, p)
  },
}
