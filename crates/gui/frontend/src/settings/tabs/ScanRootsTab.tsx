import { useState } from 'react'
import type { AppConfigDto, ScanRootEntryDto } from '@/types'
import type { ValidationErrors } from '../settingsValidation'
import { duplicatePathKey, normalizeDistro } from '../settingsValidation'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { pickDirectory, scanRoot } from '@/api'
import { Folder, Plus, Radar, Trash2 } from 'lucide-react'

interface Props {
  draft: AppConfigDto
  onChange: (
    next: AppConfigDto | ((current: AppConfigDto) => AppConfigDto),
    immediate: boolean,
  ) => void
  errors: ValidationErrors
}

export function ScanRootsTab({ draft, onChange, errors }: Props) {
  const [scanningIndex, setScanningIndex] = useState<number | null>(null)
  const [scanMessages, setScanMessages] = useState<Record<number, string>>({})

  const update = (i: number, patch: Partial<ScanRootEntryDto>, immediate: boolean) => {
    onChange(
      current => ({
        ...current,
        scan_roots: current.scan_roots.map((s, idx) => (idx === i ? { ...s, ...patch } : s)),
      }),
      immediate,
    )
  }

  const remove = (i: number) => {
    onChange(
      current => ({
        ...current,
        scan_roots: current.scan_roots.filter((_, idx) => idx !== i),
      }),
      true,
    )
  }

  const add = () => {
    onChange(
      current => ({
        ...current,
        scan_roots: [...current.scan_roots, { path: '', depth: 3, exclude: [], distro: null }],
      }),
      true,
    )
  }

  const browse = async (i: number) => {
    const picked = await pickDirectory()
    if (picked) update(i, { path: picked }, true)
  }

  const repoKey = (path: string, distro: string | null) => {
    return duplicatePathKey(path, normalizeDistro(distro))
  }

  const registerDiscovered = async (i: number) => {
    const root = draft.scan_roots[i]
    if (!root || errors[`scan_roots[${i}].path`] || errors[`scan_roots[${i}].depth`]) return

    setScanningIndex(i)
    setScanMessages(messages => ({ ...messages, [i]: 'Scanning...' }))
    try {
      const discovered = await scanRoot(root.path, root.depth, root.exclude, root.distro)
      let added = 0
      let skipped = 0
      let ignored = false
      onChange(current => {
        const currentRoot = current.scan_roots[i]
        if (
          !currentRoot ||
          currentRoot.path !== root.path ||
          currentRoot.depth !== root.depth ||
          currentRoot.exclude.join('\0') !== root.exclude.join('\0') ||
          normalizeDistro(currentRoot.distro) !== normalizeDistro(root.distro)
        ) {
          ignored = true
          return current
        }

        const existing = new Set(current.repositories.map(repo => repoKey(repo.path, repo.distro)))
        const additions = discovered.filter(repo => {
          const key = repoKey(repo.path, repo.distro)
          if (existing.has(key)) return false
          existing.add(key)
          return true
        })
        added = additions.length
        skipped = discovered.length - additions.length
        return additions.length > 0
          ? { ...current, repositories: [...current.repositories, ...additions] }
          : current
      }, true)

      setScanMessages(messages => ({
        ...messages,
        [i]: ignored
          ? 'Scan result ignored because the root changed'
          : `Found ${discovered.length}, added ${added}${skipped > 0 ? `, skipped ${skipped}` : ''}`,
      }))
    } catch (e) {
      const message =
        typeof e === 'object' && e !== null && 'message' in e
          ? String((e as { message: unknown }).message)
          : String(e)
      setScanMessages(messages => ({ ...messages, [i]: message }))
    } finally {
      setScanningIndex(null)
    }
  }

  return (
    <div className="space-y-3">
      {draft.scan_roots.length === 0 && (
        <p className="text-sm text-gray-500 dark:text-gray-400">No scan roots registered.</p>
      )}
      {draft.scan_roots.map((root, i) => {
        const pathError = errors[`scan_roots[${i}].path`]
        const depthError = errors[`scan_roots[${i}].depth`]
        return (
          <div key={i} className="border border-gray-200 dark:border-gray-700 rounded-md p-3 space-y-2 bg-white dark:bg-gray-900">
            <div className="flex gap-2 items-end">
              <div className="flex-1 space-y-1">
                <Label htmlFor={`scan-root-path-${i}`}>Path</Label>
                <div className="flex gap-2">
                  <Input
                    id={`scan-root-path-${i}`}
                    className="flex-1"
                    value={root.path}
                    invalid={!!pathError}
                    onChange={e => update(i, { path: e.target.value }, false)}
                  />
                  <Button
                    variant="outline"
                    size="default"
                    onClick={() => browse(i)}
                    aria-label="Browse scan root path"
                  >
                    <Folder className="h-4 w-4" />
                  </Button>
                </div>
              </div>
              <div className="w-24 space-y-1">
                <Label htmlFor={`scan-root-depth-${i}`}>Depth</Label>
                <Input
                  id={`scan-root-depth-${i}`}
                  type="number"
                  min={0}
                  max={32}
                  value={root.depth}
                  invalid={!!depthError}
                  onChange={e => update(i, { depth: Number(e.target.value) }, false)}
                />
              </div>
              <Button
                variant="ghost"
                size="icon"
                onClick={() => remove(i)}
                aria-label="Remove scan root"
              >
                <Trash2 className="h-4 w-4 text-red-600" />
              </Button>
            </div>
            {(pathError || depthError) && (
              <p className="text-xs text-red-600 dark:text-red-400">{pathError ?? depthError}</p>
            )}
            <div className="space-y-1">
              <Label htmlFor={`scan-root-distro-${i}`}>Distro (WSL)</Label>
              <Input
                id={`scan-root-distro-${i}`}
                value={root.distro ?? ''}
                placeholder="(empty = native, or default WSL for / paths)"
                onChange={e => update(i, { distro: normalizeDistro(e.target.value) }, false)}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor={`scan-root-exclude-${i}`}>Exclude (comma-separated)</Label>
              <Input
                id={`scan-root-exclude-${i}`}
                value={root.exclude.join(', ')}
                onChange={e => {
                  const exclude = e.target.value
                    .split(',')
                    .map(s => s.trim())
                    .filter(s => s !== '')
                  update(i, { exclude }, false)
                }}
              />
            </div>
            <div className="flex items-center gap-3">
              <Button
                variant="outline"
                size="sm"
                onClick={() => registerDiscovered(i)}
                disabled={
                  scanningIndex !== null || !!pathError || !!depthError || root.path.trim() === ''
                }
              >
                <Radar className="h-4 w-4" />
                Scan
              </Button>
              {scanMessages[i] && (
                <span className="text-xs text-gray-500 dark:text-gray-400">{scanMessages[i]}</span>
              )}
            </div>
          </div>
        )
      })}
      <Button variant="outline" size="sm" onClick={add}>
        <Plus className="h-4 w-4" />
        Add scan root
      </Button>
    </div>
  )
}
