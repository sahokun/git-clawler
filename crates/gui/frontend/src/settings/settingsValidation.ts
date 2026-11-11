// Frontend mirror of the Rust validation rules. The truth predicates and ranges
// live in crates/core/src/config.rs (wsl_path_requirement_met, MIN/MAX_* consts)
// and are shared by crates/gui/src/commands.rs::validate_for_save. This file
// cannot call Rust (it runs in the WebView), so it stays a mirror for instant
// inline feedback. When a rule changes, update all three: core / gui / this file.
// The numeric constants below must match core's MIN/MAX_* values.
import type { AppConfigDto } from '@/types'

export type ValidationErrors = Partial<Record<string, string>>

const ENV_VAR_RE = /^[A-Za-z_][A-Za-z0-9_]*$/
const MIN_WINDOW_WIDTH = 320
const MAX_WINDOW_WIDTH = 7680
const MIN_WINDOW_HEIGHT = 240
const MAX_WINDOW_HEIGHT = 4320
const MIN_COLUMN_WIDTH = 40
const MAX_COLUMN_WIDTH = 1600
const MIN_REFRESH_INTERVAL_SECS = 1
const MAX_REFRESH_INTERVAL_SECS = 86400
const MIN_COOLDOWN_SECS = 1
const MAX_COOLDOWN_SECS = 86400
const MAX_SCAN_DEPTH = 32

export function normalizeDistro(v: string | null | undefined): string | null {
  if (!v) return null
  const trimmed = v.trim()
  return trimmed === '' ? null : trimmed
}

export function duplicatePathKey(path: string, distro: string | null | undefined): string {
  const normalizedDistro = normalizeDistro(distro)
  const normalizedPath = normalizedDistro !== null || path.startsWith('/')
    ? normalizePosixPath(path)
    : normalizeWindowsNativePath(path)
  return `${normalizedDistro ?? ''}|${normalizedPath}`
}

function normalizeWindowsNativePath(path: string): string {
  const normalized = path
    .replace(/^\\\\\?\\UNC\\/i, '\\\\')
    .replace(/^\\\\\?\\/i, '')
    .replace(/\//g, '\\')
    .toLowerCase()
  const drivePrefix = normalized.match(/^[a-z]:\\/)?.[0]
  if (drivePrefix) {
    return joinNormalizedPrefix(drivePrefix, normalizePathParts(normalized.slice(drivePrefix.length), '\\'), '\\')
  }
  if (normalized.startsWith('\\\\')) {
    const parts = normalized.slice(2).split('\\')
    if (parts.length >= 2 && parts[0] && parts[1]) {
      const prefix = `\\\\${parts[0]}\\${parts[1]}\\`
      return joinNormalizedPrefix(prefix, normalizePathParts(parts.slice(2).join('\\'), '\\'), '\\')
    }
    return normalized
  }
  if (normalized.startsWith('\\')) {
    return joinNormalizedPrefix('\\', normalizePathParts(normalized.slice(1), '\\'), '\\')
  }
  return normalizePathParts(normalized, '\\')
}

function normalizePosixPath(path: string): string {
  if (path.startsWith('/')) {
    return joinNormalizedPrefix('/', normalizePathParts(path.slice(1), '/'), '/')
  }
  return normalizePathParts(path, '/')
}

function normalizePathParts(path: string, separator: '/' | '\\'): string {
  const parts: string[] = []
  for (const part of path.split(separator)) {
    if (part === '' || part === '.') continue
    parts.push(part)
  }
  return parts.join(separator) || '.'
}

function joinNormalizedPrefix(prefix: string, body: string, separator: '/' | '\\'): string {
  if (body === '.') return prefix
  return `${prefix}${prefix.endsWith(separator) ? '' : separator}${body}`
}

export function validateConfig(cfg: AppConfigDto): ValidationErrors {
  const errors: ValidationErrors = {}

  // general.refresh_interval_secs
  const rs = cfg.general.refresh_interval_secs
  if (!Number.isFinite(rs) || !Number.isInteger(rs) || rs < MIN_REFRESH_INTERVAL_SECS || rs > MAX_REFRESH_INTERVAL_SECS) {
    errors['general.refresh_interval_secs'] = `must be ${MIN_REFRESH_INTERVAL_SECS}..=${MAX_REFRESH_INTERVAL_SECS}`
  }

  // notifications.cooldown_secs
  const cooldownSecs = cfg.notifications.cooldown_secs
  if (!Number.isFinite(cooldownSecs) || !Number.isInteger(cooldownSecs) || cooldownSecs < MIN_COOLDOWN_SECS || cooldownSecs > MAX_COOLDOWN_SECS) {
    errors['notifications.cooldown_secs'] = `must be ${MIN_COOLDOWN_SECS}..=${MAX_COOLDOWN_SECS}`
  }

  // repositories: path required, no duplicate (distro, path) pair
  // (existence is checked by Rust safety net)
  const seenRepoKeys = new Set<string>()
  cfg.repositories.forEach((r, i) => {
    if (!r.path || r.path.trim() === '') {
      errors[`repositories[${i}].path`] = 'path is required'
      return
    }
    const nd = normalizeDistro(r.distro)
    if (nd !== null && !r.path.startsWith('/')) {
      errors[`repositories[${i}].path`] = 'WSL entry requires a Linux absolute path (must start with /)'
      return
    }
    const key = duplicatePathKey(r.path, nd)
    if (seenRepoKeys.has(key)) {
      errors[`repositories[${i}].path`] = 'duplicate (distro, path)'
    }
    seenRepoKeys.add(key)
  })

  // scan_roots: path required, depth 0..=32, distro entries require Linux paths
  const seenScanRootKeys = new Set<string>()
  cfg.scan_roots.forEach((s, i) => {
    if (!s.path || s.path.trim() === '') {
      errors[`scan_roots[${i}].path`] = 'path is required'
    }
    const nd = normalizeDistro(s.distro)
    if (nd !== null && !s.path.startsWith('/')) {
      errors[`scan_roots[${i}].path`] = 'WSL scan root requires a Linux absolute path (must start with /)'
    }
    const key = duplicatePathKey(s.path, nd)
    if (seenScanRootKeys.has(key)) {
      errors[`scan_roots[${i}].path`] = 'duplicate scan root (distro, path)'
    }
    seenScanRootKeys.add(key)
    if (!Number.isFinite(s.depth) || s.depth < 0 || s.depth > MAX_SCAN_DEPTH) {
      errors[`scan_roots[${i}].depth`] = `depth must be 0..=${MAX_SCAN_DEPTH}`
    }
  })

  // github.token_env: regex only when Some and non-empty
  if (cfg.github.token_env != null && cfg.github.token_env !== '') {
    if (!ENV_VAR_RE.test(cfg.github.token_env)) {
      errors['github.token_env'] = 'must match ^[A-Za-z_][A-Za-z0-9_]*$'
    }
  }

  // gui.global_shortcut: when Some, it must be non-empty after trim
  if (cfg.gui.theme !== 'dark' && cfg.gui.theme !== 'light') {
    errors['gui.theme'] = 'must be dark or light'
  }
  if (cfg.gui.global_shortcut != null && cfg.gui.global_shortcut.trim() === '') {
    errors['gui.global_shortcut'] = 'must not be blank'
  }
  if (cfg.gui.window_width != null) {
    const width = cfg.gui.window_width
    if (!Number.isFinite(width) || width < MIN_WINDOW_WIDTH || width > MAX_WINDOW_WIDTH) {
      errors['gui.window_width'] = `must be ${MIN_WINDOW_WIDTH}..=${MAX_WINDOW_WIDTH}`
    }
  }
  if (cfg.gui.window_height != null) {
    const height = cfg.gui.window_height
    if (!Number.isFinite(height) || height < MIN_WINDOW_HEIGHT || height > MAX_WINDOW_HEIGHT) {
      errors['gui.window_height'] = `must be ${MIN_WINDOW_HEIGHT}..=${MAX_WINDOW_HEIGHT}`
    }
  }
  Object.entries(cfg.gui.column_widths).forEach(([column, width]) => {
    if (!Number.isFinite(width) || width < MIN_COLUMN_WIDTH || width > MAX_COLUMN_WIDTH) {
      errors[`gui.column_widths.${column}`] = `must be ${MIN_COLUMN_WIDTH}..=${MAX_COLUMN_WIDTH}`
    }
  })

  return errors
}

export function hasErrors(errors: ValidationErrors): boolean {
  return Object.keys(errors).length > 0
}
