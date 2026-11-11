/**
 * Convert a KeyboardEvent into a normalized shortcut string.
 *
 * - Modifier order: Ctrl → Alt → Shift → Meta → Key
 * - Modifier-only key presses are ignored (return undefined, not a string).
 * - Escape returns null as a cancel signal.
 */
export function captureShortcut(e: KeyboardEvent): string | null | undefined {
  if (e.key === 'Escape') return null

  const key = normalizeShortcutKey(e.key)
  // Ignore modifier-only key presses.
  if (key === 'Control' || key === 'Alt' || key === 'Shift' || key === 'Meta') {
    return undefined
  }

  const parts: string[] = []
  if (e.ctrlKey) parts.push('Ctrl')
  if (e.altKey) parts.push('Alt')
  if (e.shiftKey) parts.push('Shift')
  if (e.metaKey) parts.push('Meta')

  parts.push(key)

  if (parts.length < 2) {
    // Do not treat unmodified single keys as shortcuts.
    return undefined
  }

  return parts.join('+')
}

export function normalizeShortcutKey(key: string): string {
  switch (key) {
    case 'ArrowUp':
      return 'Up'
    case 'ArrowDown':
      return 'Down'
    case 'ArrowLeft':
      return 'Left'
    case 'ArrowRight':
      return 'Right'
    case 'Spacebar':
    case ' ':
      return 'Space'
    default:
      return key.length === 1 ? key.toUpperCase() : key
  }
}
