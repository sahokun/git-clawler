import type { AppConfigDto, RepoEntryDto } from '@/types'
import type { ValidationErrors } from '../settingsValidation'
import { normalizeDistro } from '../settingsValidation'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { pickDirectory, removeRepository } from '@/api'
import { Folder, Plus, Trash2 } from 'lucide-react'

interface Props {
  draft: AppConfigDto
  onChange: (
    next: AppConfigDto | ((current: AppConfigDto) => AppConfigDto),
    immediate: boolean,
  ) => void
  errors: ValidationErrors
}

export function RepositoriesTab({ draft, onChange, errors }: Props) {
  const update = (i: number, patch: Partial<RepoEntryDto>, immediate: boolean) => {
    onChange(
      current => ({
        ...current,
        repositories: current.repositories.map((r, idx) => (idx === i ? { ...r, ...patch } : r)),
      }),
      immediate,
    )
  }

  const removeFromDraft = (path: string, distro: string | null) => {
    onChange(
      current => ({
        ...current,
        repositories: current.repositories.filter(r => !(r.path === path && r.distro === distro)),
      }),
      true,
    )
  }

  // Saved entries are removed through the backend so an auto scan root gets an
  // exclusion and does not re-add them; unsaved rows only leave the draft.
  const remove = async (i: number) => {
    const entry = draft.repositories[i]
    if (!entry) return
    if (entry.path.trim() !== '') {
      try {
        await removeRepository(entry.path, entry.distro)
      } catch {
        // Not registered yet (unsaved row): fall through to the draft change.
      }
    }
    removeFromDraft(entry.path, entry.distro)
  }

  const add = () => {
    onChange(
      current => ({
        ...current,
        repositories: [
          ...current.repositories,
          { name: null, path: '', fetch: null, distro: null },
        ],
      }),
      true,
    )
  }

  const browse = async (i: number) => {
    const picked = await pickDirectory()
    if (picked) update(i, { path: picked }, true)
  }

  return (
    <div className="space-y-3">
      {draft.repositories.length === 0 && (
        <p className="text-sm text-gray-500 dark:text-gray-400">No repositories registered.</p>
      )}
      {draft.repositories.map((repo, i) => {
        const pathError = errors[`repositories[${i}].path`]
        return (
          <div key={i} className="border border-gray-200 dark:border-gray-700 rounded-md p-3 space-y-2 bg-white dark:bg-gray-900">
            <div className="flex gap-2 items-end">
              <div className="flex-1 space-y-1">
                <Label htmlFor={`repo-name-${i}`}>Name</Label>
                <Input
                  id={`repo-name-${i}`}
                  value={repo.name ?? ''}
                  placeholder="(optional)"
                  onChange={e =>
                    update(i, { name: e.target.value === '' ? null : e.target.value }, false)
                  }
                />
              </div>
              <div className="flex items-center gap-2 pb-1">
                <Checkbox
                  id={`repo-fetch-${i}`}
                  checked={repo.fetch === true}
                  onCheckedChange={checked =>
                    update(i, { fetch: checked === true ? true : null }, true)
                  }
                />
                <Label htmlFor={`repo-fetch-${i}`}>fetch</Label>
              </div>
              <Button
                variant="ghost"
                size="icon"
                onClick={() => remove(i)}
                aria-label="Remove repository"
              >
                <Trash2 className="h-4 w-4 text-red-600" />
              </Button>
            </div>
            <div className="space-y-1">
              <Label htmlFor={`repo-path-${i}`}>Path</Label>
              <div className="flex gap-2">
                <Input
                  id={`repo-path-${i}`}
                  className="flex-1"
                  value={repo.path}
                  invalid={!!pathError}
                  onChange={e => update(i, { path: e.target.value }, false)}
                />
                <Button variant="outline" size="default" onClick={() => browse(i)}>
                  <Folder className="h-4 w-4" />
                  Browse
                </Button>
              </div>
              {pathError && <p className="text-xs text-red-600 dark:text-red-400">{pathError}</p>}
            </div>
            <div className="space-y-1">
              <Label htmlFor={`repo-distro-${i}`}>Distro (WSL)</Label>
              <Input
                id={`repo-distro-${i}`}
                value={repo.distro ?? ''}
                placeholder="(empty = native, or default WSL for / paths)"
                onChange={e =>
                  update(i, { distro: normalizeDistro(e.target.value) }, false)
                }
              />
            </div>
          </div>
        )
      })}
      <Button variant="outline" size="sm" onClick={add}>
        <Plus className="h-4 w-4" />
        Add repository
      </Button>
    </div>
  )
}
