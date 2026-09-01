// Is this an installed app, or a browser tab?
//
// The difference decides whether the client is entitled to pull the whole vault down for
// offline use. Installing is a deliberate act on a device you own, and the payoff — the vault
// readable on a plane — is what it is for. A tab is not: someone opening their notes on a
// borrowed laptop should not silently leave a complete copy in that machine's IndexedDB, and
// on a slow connection should not spend the bandwidth on notes they did not ask for.
//
// The Tauri shells report `browser` here and are excluded, which is right for a different
// reason: their relay already has the whole vault on local disk.

/** True when running from a home-screen icon or an installed-app window. */
export function isInstalled(): boolean {
  try {
    // The standard signal. `fullscreen` and `minimal-ui` are the other installed display
    // modes; a manifest may ask for any of them, and none of them is a tab.
    for (const mode of ['standalone', 'fullscreen', 'minimal-ui']) {
      if (matchMedia(`(display-mode: ${mode})`).matches) return true
    }
    // iOS home-screen apps predate `display-mode` and older versions still answer only here.
    return (navigator as Navigator & { standalone?: boolean }).standalone === true
  } catch {
    return false
  }
}
