import { describe, expect, it } from 'vitest'
import { captureShortcut, normalizeShortcutKey } from '../shortcutCapture'

function keyboardEvent(key: string, init: Partial<KeyboardEventInit> = {}) {
  return new KeyboardEvent('keydown', { key, ...init })
}

describe('normalizeShortcutKey', () => {
  it.each([
    ['ArrowUp', 'Up'],
    ['ArrowDown', 'Down'],
    ['ArrowLeft', 'Left'],
    ['ArrowRight', 'Right'],
    [' ', 'Space'],
    ['Spacebar', 'Space'],
    ['PageUp', 'PageUp'],
    ['PageDown', 'PageDown'],
  ])('normalizes %s to %s', (input, expected) => {
    expect(normalizeShortcutKey(input)).toBe(expected)
  })

  it('uppercases one-character keys', () => {
    expect(normalizeShortcutKey('g')).toBe('G')
  })
})

describe('captureShortcut', () => {
  it('captures arrow keys in canonical form', () => {
    expect(captureShortcut(keyboardEvent('ArrowUp', { ctrlKey: true }))).toBe('Ctrl+Up')
  })

  it('captures space in canonical form', () => {
    expect(captureShortcut(keyboardEvent(' ', { ctrlKey: true, shiftKey: true }))).toBe(
      'Ctrl+Shift+Space',
    )
  })

  it('captures PageUp without breaking camel case', () => {
    expect(captureShortcut(keyboardEvent('PageUp', { ctrlKey: true }))).toBe('Ctrl+PageUp')
  })
})
