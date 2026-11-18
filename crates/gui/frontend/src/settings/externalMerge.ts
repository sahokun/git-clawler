import type { AppConfigDto, RepoEntryDto, ScanRootEntryDto } from '@/types'
import { duplicatePathKey, normalizeDistro } from './settingsValidation'

const repoKey = (r: RepoEntryDto) => duplicatePathKey(r.path, normalizeDistro(r.distro))
const rootKey = (r: ScanRootEntryDto) => duplicatePathKey(r.path, normalizeDistro(r.distro))

/**
 * Merge repositories changed on disk by the backend (discovery or removal)
 * into an open Settings draft without discarding the user's edits.
 *
 * - Entries on disk are kept in disk order; a matching draft entry keeps its
 *   edited name and fetch override.
 * - Draft entries unknown to both the previous and current disk state are
 *   unsaved user additions and are kept.
 * - Draft entries that were on disk before but are gone now were removed or
 *   pruned by the backend and are dropped.
 * - Scan-root exclusions come from disk for roots present in both.
 */
export function mergeExternalRepositories(
  draft: AppConfigDto,
  previousDisk: AppConfigDto | null,
  disk: AppConfigDto,
): AppConfigDto {
  const draftByKey = new Map(draft.repositories.map(r => [repoKey(r), r]))
  const diskKeys = new Set(disk.repositories.map(repoKey))
  const previousKeys = new Set((previousDisk ?? disk).repositories.map(repoKey))
  const repositories = [
    ...disk.repositories.map(d => {
      const edited = draftByKey.get(repoKey(d))
      return edited ? { ...d, name: edited.name, fetch: edited.fetch } : d
    }),
    ...draft.repositories.filter(r => !diskKeys.has(repoKey(r)) && !previousKeys.has(repoKey(r))),
  ]
  const diskRoots = new Map(disk.scan_roots.map(r => [rootKey(r), r]))
  const scan_roots = draft.scan_roots.map(r => {
    const fromDisk = diskRoots.get(rootKey(r))
    return fromDisk ? { ...r, exclude: fromDisk.exclude } : r
  })
  return { ...draft, repositories, scan_roots }
}
