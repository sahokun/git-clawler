import { describe, it, expect } from 'vitest'
import { validateConfig, normalizeDistro, duplicatePathKey } from '../settingsValidation'
import type { AppConfigDto } from '@/types'

function baseConfig(): AppConfigDto {
  return {
    general: { refresh_interval_secs: 30, auto_fetch: false },
    repositories: [],
    scan_roots: [],
    github: { token_env: null, enabled: false, enable_issues: true, enable_prs: true },
    notifications: { enabled: false, cooldown_secs: 300 },
    gui: {
      theme: 'dark',
      start_in_tray: false,
      hide_from_taskbar: false,
      close_to_tray: false,
      global_shortcut: null,
      window_x: null,
      window_y: null,
      window_width: null,
      window_height: null,
      window_maximized: null,
      column_widths: {},
    },
  }
}

describe('validateConfig: repository (distro, path) duplicate key', () => {
  it('treats same path with different distros as distinct entries', () => {
    const cfg = baseConfig()
    cfg.repositories = [
      { name: null, path: '/home/me/proj', fetch: null, distro: null },
      { name: null, path: '/home/me/proj', fetch: null, distro: 'Ubuntu' },
    ]
    const errors = validateConfig(cfg)
    expect(errors['repositories[0].path']).toBeUndefined()
    expect(errors['repositories[1].path']).toBeUndefined()
  })

  it('flags duplicates that share both distro (null) and path', () => {
    const cfg = baseConfig()
    cfg.repositories = [
      { name: null, path: '/x', fetch: null, distro: null },
      { name: null, path: '/x', fetch: null, distro: null },
    ]
    const errors = validateConfig(cfg)
    expect(errors['repositories[1].path']).toBe('duplicate (distro, path)')
  })

  it('flags duplicates that share the same distro string and path', () => {
    const cfg = baseConfig()
    cfg.repositories = [
      { name: null, path: '/x', fetch: null, distro: 'Ubuntu' },
      { name: null, path: '/x', fetch: null, distro: 'Ubuntu' },
    ]
    const errors = validateConfig(cfg)
    expect(errors['repositories[1].path']).toBe('duplicate (distro, path)')
  })
})

describe('validateConfig: gui preferences', () => {
  it('rejects unknown theme values', () => {
    const cfg = baseConfig()
    cfg.gui.theme = 'blue' as AppConfigDto['gui']['theme']
    const errors = validateConfig(cfg)
    expect(errors['gui.theme']).toBe('must be dark or light')
  })

  it('rejects invalid window size', () => {
    const cfg = baseConfig()
    cfg.gui.window_width = 1
    cfg.gui.window_height = 1
    const errors = validateConfig(cfg)
    expect(errors['gui.window_width']).toContain('320')
    expect(errors['gui.window_height']).toContain('240')
  })

  it('rejects invalid column widths', () => {
    const cfg = baseConfig()
    cfg.gui.column_widths = { branch: 1 }
    const errors = validateConfig(cfg)
    expect(errors['gui.column_widths.branch']).toContain('40')
  })
})

describe('validateConfig: notifications', () => {
  it('rejects cooldown below minimum', () => {
    const cfg = baseConfig()
    cfg.notifications.cooldown_secs = 0
    const errors = validateConfig(cfg)
    expect(errors['notifications.cooldown_secs']).toBe('must be 1..=86400')
  })

  it('rejects cooldown above maximum', () => {
    const cfg = baseConfig()
    cfg.notifications.cooldown_secs = 86401
    const errors = validateConfig(cfg)
    expect(errors['notifications.cooldown_secs']).toBe('must be 1..=86400')
  })

  it('rejects fractional cooldown values', () => {
    const cfg = baseConfig()
    cfg.notifications.cooldown_secs = 1.5
    const errors = validateConfig(cfg)
    expect(errors['notifications.cooldown_secs']).toBe('must be 1..=86400')
  })

  it('accepts cooldown boundary values', () => {
    for (const cooldown of [1, 86400]) {
      const cfg = baseConfig()
      cfg.notifications.cooldown_secs = cooldown
      const errors = validateConfig(cfg)
      expect(errors['notifications.cooldown_secs']).toBeUndefined()
    }
  })
})

describe('normalizeDistro', () => {
  it('null → null', () => expect(normalizeDistro(null)).toBeNull())
  it('undefined → null', () => expect(normalizeDistro(undefined)).toBeNull())
  it('"" → null', () => expect(normalizeDistro('')).toBeNull())
  it('"  " → null', () => expect(normalizeDistro('  ')).toBeNull())
  it('" Ubuntu " → "Ubuntu"', () => expect(normalizeDistro(' Ubuntu ')).toBe('Ubuntu'))
})

describe('validateConfig: normalizeDistro duplicate key', () => {
  it('(null, /a) and ("", /a) are treated as duplicates', () => {
    const cfg = baseConfig()
    cfg.repositories = [
      { name: null, path: '/a', fetch: null, distro: null },
      { name: null, path: '/a', fetch: null, distro: '' },
    ]
    const errors = validateConfig(cfg)
    expect(errors['repositories[1].path']).toBe('duplicate (distro, path)')
  })

  describe('duplicatePathKey', () => {
    it('mirrors Windows native case-insensitive path identity', () => {
      expect(duplicatePathKey('\\\\?\\C:\\Users\\Me\\Repo', null))
        .toBe(duplicatePathKey('c:\\users\\me\\repo', null))
    })

    it('keeps WSL distro paths case-sensitive', () => {
      expect(duplicatePathKey('/home/me/Repo', 'Ubuntu'))
        .not.toBe(duplicatePathKey('/home/me/repo', 'Ubuntu'))
    })

    it('keeps default WSL paths case-sensitive without a distro', () => {
      expect(duplicatePathKey('/home/me/Repo', null))
        .not.toBe(duplicatePathKey('/home/me/repo', null))
    })

    it('keeps Windows UNC server/share as the normalization root', () => {
      expect(duplicatePathKey('\\\\server\\share\\.\\repo', null))
        .toBe(duplicatePathKey('\\\\server\\share\\repo', null))
      expect(duplicatePathKey('\\\\server\\share\\..\\other', null))
        .not.toBe(duplicatePathKey('\\\\server\\other', null))
    })

    it('does not collapse parent dirs across possible symlinks', () => {
      expect(duplicatePathKey('/a/link/../repo', 'Ubuntu'))
        .not.toBe(duplicatePathKey('/a/repo', 'Ubuntu'))
    })
  })
})

describe('validateConfig: WSL path must start with /', () => {
  it('rejects distro=Some + Windows path', () => {
    const cfg = baseConfig()
    cfg.repositories = [
      { name: null, path: 'C:\\Users\\foo', fetch: null, distro: 'Ubuntu' },
    ]
    const errors = validateConfig(cfg)
    expect(errors['repositories[0].path']).toContain('/')
  })

  it('accepts distro=Some + Linux path', () => {
    const cfg = baseConfig()
    cfg.repositories = [
      { name: null, path: '/home/me/proj', fetch: null, distro: 'Ubuntu' },
    ]
    const errors = validateConfig(cfg)
    expect(errors['repositories[0].path']).toBeUndefined()
  })
})

describe('validateConfig: WSL scan roots', () => {
  it('accepts distro=Some + Linux scan root path', () => {
    const cfg = baseConfig()
    cfg.scan_roots = [
      { path: '/home/me/workspace', depth: 3, exclude: [], distro: 'Ubuntu' },
    ]
    const errors = validateConfig(cfg)
    expect(errors['scan_roots[0].path']).toBeUndefined()
  })

  it('rejects distro=Some + Windows scan root path', () => {
    const cfg = baseConfig()
    cfg.scan_roots = [
      { path: 'C:\\workspace', depth: 3, exclude: [], distro: 'Ubuntu' },
    ]
    const errors = validateConfig(cfg)
    expect(errors['scan_roots[0].path']).toContain('/')
  })

  it('flags duplicate scan roots with the same normalized distro and path', () => {
    const cfg = baseConfig()
    cfg.scan_roots = [
      { path: '/home/me/workspace', depth: 3, exclude: [], distro: ' Ubuntu ' },
      { path: '/home/me/workspace', depth: 2, exclude: [], distro: 'Ubuntu' },
    ]
    const errors = validateConfig(cfg)
    expect(errors['scan_roots[1].path']).toBe('duplicate scan root (distro, path)')
  })
})
