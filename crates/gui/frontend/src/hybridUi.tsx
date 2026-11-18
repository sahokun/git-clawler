import { useId, useLayoutEffect, useRef, useState } from 'react'
import type { PointerEvent as ReactPointerEvent, ReactNode } from 'react'
import {
  AlertTriangle,
  ArrowDown,
  ArrowUp,
  Check,
  Clock,
  ExternalLink,
  FolderOpen,
  GitBranch,
  GitFork,
  Monitor,
  Server,
  Trash2,
  X,
} from 'lucide-react'
import type { RepoRow, RepoStatusDto } from './types'
import { Button } from './components/ui/button'

export type ColumnKey =
  | 'repository'
  | 'branch'
  | 'uncommitted'
  | 'push'
  | 'aheadBehind'
  | 'conflict'
  | 'issues'
  | 'prs'
  | 'lastScan'

export type HostKind = 'native' | 'wsl'
export type StatusTone = 'ok' | 'warn' | 'info' | 'danger' | 'muted'

export interface ColumnDefinition {
  key: ColumnKey
  label: string
  tooltip: string
  defaultWidth: number
  align: 'left' | 'center'
  flexible?: boolean
}

export interface RepoViewModel {
  row: RepoRow
  repository: string
  displayName: string
  parentPath: string
  host: HostKind
  hostLabel: string
  state: 'clean' | 'dirty' | 'behind' | 'conflict' | 'error'
  /** The registered path no longer exists (removal is up to the user). */
  notFound: boolean
  /** Main repository path when this row is a linked worktree. */
  worktreeOf: string | null
  /** Rendered under its main repository (set by {@link groupWorktrees}). */
  nested?: boolean
  tone: StatusTone
  status: RepoStatusDto | null
  error: string | null
  branch: string
  uncommitted: number
  push: number
  ahead: number
  behind: number
  issues: number | null
  prs: number | null
  conflictLabel: string
  conflictTooltip: string
  lastScan: string
  remoteUrl: string | null
  githubUrl: string | null
  searchable: string
}

export interface KpiTotals {
  repos: number
  worktrees: number
  uncommitted: number
  push: number
  behind: number
  conflicts: number
  errors: number
}

export const TABLE_COLUMNS: ColumnDefinition[] = [
  {
    key: 'repository',
    label: 'Repository',
    tooltip: 'Full path of the monitored repository. A leading * marks the current working directory.',
    defaultWidth: 420,
    align: 'left',
  },
  {
    key: 'branch',
    label: 'Branch',
    tooltip: 'Currently checked out Git branch. A dash is shown when it cannot be detected.',
    defaultWidth: 140,
    align: 'left',
  },
  {
    key: 'uncommitted',
    label: 'Uncommitted',
    tooltip: 'Number of files with uncommitted working tree or index changes.',
    defaultWidth: 124,
    align: 'center',
  },
  {
    key: 'push',
    label: 'Push',
    tooltip: 'Number of local branches that need to be pushed, including branches without upstream.',
    defaultWidth: 72,
    align: 'center',
  },
  {
    key: 'aheadBehind',
    label: 'A / B',
    tooltip: 'Commits ahead of upstream / commits behind upstream for the current branch.',
    defaultWidth: 84,
    align: 'center',
  },
  {
    key: 'conflict',
    label: 'Conflict',
    tooltip: 'Pull conflict prediction. Warning means possible conflict, check means likely safe, dash means unknown.',
    defaultWidth: 88,
    align: 'center',
  },
  {
    key: 'issues',
    label: 'Issues',
    tooltip: 'Open GitHub issue count when GitHub integration is enabled. A dash means unavailable.',
    defaultWidth: 76,
    align: 'center',
  },
  {
    key: 'prs',
    label: 'PRs',
    tooltip: 'Open GitHub pull request count when GitHub integration is enabled. A dash means unavailable.',
    defaultWidth: 64,
    align: 'center',
  },
  {
    key: 'lastScan',
    label: 'Last scan',
    tooltip: 'Local time when this repository was last scanned.',
    defaultWidth: 112,
    align: 'center',
  },
]

function formatLastScan(value: string): string {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '-'
  const hours = String(date.getHours()).padStart(2, '0')
  const minutes = String(date.getMinutes()).padStart(2, '0')
  const seconds = String(date.getSeconds()).padStart(2, '0')
  return `${hours}:${minutes}:${seconds}`
}

function formatCommitTime(value: string): string {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return value
  return date.toLocaleString()
}

function basename(path: string): string {
  const normalized = path.replace(/[\\/]+$/, '')
  const parts = normalized.split(/[\\/]/)
  return parts[parts.length - 1] || normalized
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function parentPath(path: string, name: string): string {
  const normalized = path.replace(/[\\/]+$/, '')
  const suffix = new RegExp(`[\\\\/]?${escapeRegExp(name)}$`)
  return normalized.replace(suffix, '') || path
}

function conflictLabel(status: RepoStatusDto): string {
  if (status.conflict_predicted === null) return '-'
  return status.conflict_predicted ? '⚠' : '✓'
}

export function githubUrlFromRemote(remoteUrl: string | null): string | null {
  if (!remoteUrl) return null
  const trimmed = remoteUrl.trim()
  const path = trimmed.startsWith('https://github.com/')
    ? trimmed.slice('https://github.com/'.length)
    : trimmed.startsWith('http://github.com/')
      ? trimmed.slice('http://github.com/'.length)
      : trimmed.startsWith('git@github.com:')
        ? trimmed.slice('git@github.com:'.length)
        : trimmed.startsWith('ssh://git@github.com/')
          ? trimmed.slice('ssh://git@github.com/'.length)
          : null
  if (!path) return null
  const parts = path.replace(/\/+$/, '').split('/')
  if (parts.length !== 2) return null
  const owner = parts[0]
  const repo = parts[1].replace(/\.git$/, '')
  if (!/^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$/.test(owner)) return null
  if (!/^[A-Za-z0-9._-]{1,100}$/.test(repo)) return null
  return `https://github.com/${owner}/${repo}`
}

export function repoKey(row: RepoRow): string {
  return JSON.stringify([isWslRepoPath(row.path) ? 'wsl' : 'native', row.distro ?? null, row.path])
}

export function isWslRepoPath(path: string): boolean {
  return path.startsWith('/')
}

function hostLabel(row: RepoRow, host: HostKind): string {
  if (host === 'native') return 'Native'
  return row.distro ? `WSL · ${row.distro}` : 'WSL · default'
}

export function hasChanges(view: RepoViewModel): boolean {
  return view.error !== null
    || view.uncommitted > 0
    || view.push > 0
    || view.ahead > 0
    || view.behind > 0
    || view.status?.conflict_predicted === true
    || Boolean(view.status?.fetch_warning)
}

export function toViewModel(row: RepoRow): RepoViewModel {
  const host: HostKind = isWslRepoPath(row.path) ? 'wsl' : 'native'
  const displayName = row.name || basename(row.path)
  const repository = row.is_current ? `* ${row.path}` : row.path

  const worktreeOf = row.worktree_of ?? null
  if (row.status.kind !== 'ok') {
    const notFound = row.status.kind === 'not_found'
    return {
      row,
      repository,
      displayName,
      parentPath: parentPath(row.path, displayName),
      host,
      hostLabel: hostLabel(row, host),
      state: 'error',
      notFound,
      worktreeOf,
      tone: 'danger',
      status: null,
      error: row.status.value,
      branch: notFound ? 'NOT FOUND' : 'ERROR',
      uncommitted: 0,
      push: 0,
      ahead: 0,
      behind: 0,
      issues: null,
      prs: null,
      conflictLabel: '-',
      conflictTooltip: 'Conflict prediction is unavailable because the scan failed.',
      lastScan: formatLastScan(row.scanned_at),
      remoteUrl: row.remote_url,
      githubUrl: githubUrlFromRemote(row.remote_url),
      searchable: `${row.path} ${displayName} ${row.status.value} ${host}${notFound ? ' not found' : ''}${worktreeOf ? ' worktree' : ''}`.toLowerCase(),
    }
  }

  const status = row.status.value
  const hasConflict = status.conflict_predicted === true
  const state = hasConflict
    ? 'conflict'
    : status.uncommitted_files > 0 || status.unpushed_branches.length > 0
      ? 'dirty'
      : status.behind > 0
        ? 'behind'
        : 'clean'
  const tone: StatusTone = hasConflict
    ? 'danger'
    : status.uncommitted_files > 0
      ? 'warn'
      : status.unpushed_branches.length > 0 || status.ahead > 0 || status.behind > 0
        ? 'info'
        : 'ok'

  return {
    row,
    repository,
    displayName,
    parentPath: parentPath(row.path, displayName),
    host,
    hostLabel: hostLabel(row, host),
    state,
    notFound: false,
    worktreeOf,
    tone,
    status,
    error: null,
    branch: status.current_branch ?? '-',
    uncommitted: status.uncommitted_files,
    push: status.unpushed_branches.length,
    ahead: status.ahead,
    behind: status.behind,
    issues: status.open_issues,
    prs: status.open_prs,
    conflictLabel: conflictLabel(status),
    conflictTooltip: status.conflict_predicted === null
      ? 'Prediction is unavailable because fetch has not run or upstream information is missing.'
      : status.conflict_predicted
        ? 'Pull may touch files that also changed locally.'
        : 'The current remote-tracking refs indicate a low pull conflict risk.',
    lastScan: formatLastScan(row.scanned_at),
    remoteUrl: row.remote_url,
    githubUrl: githubUrlFromRemote(row.remote_url),
    searchable: `${row.path} ${displayName} ${status.current_branch ?? ''} ${host} ${row.distro ?? ''}`.toLowerCase(),
  }
}

export function matchesSearch(view: RepoViewModel, query: string): boolean {
  const terms = query.trim().toLowerCase().split(/\s+/).filter(Boolean)
  if (terms.length === 0) return true

  return terms.every(term => {
    if (term === 'has:conflict') return view.status?.conflict_predicted === true
    if (term === 'has:changes') return hasChanges(view)
    if (term === 'has:error') return view.error !== null
    if (term === 'host:wsl') return view.host === 'wsl'
    if (term === 'host:native' || term === 'host:win' || term === 'host:windows') return view.host === 'native'
    if (term.startsWith('branch:')) return view.branch.toLowerCase().includes(term.slice('branch:'.length))
    return view.searchable.includes(term)
  })
}

export function summarize(views: RepoViewModel[]): KpiTotals {
  return views.reduce<KpiTotals>((acc, view) => {
    if (view.worktreeOf) acc.worktrees += 1
    else acc.repos += 1
    acc.uncommitted += view.uncommitted
    acc.push += view.push
    acc.behind += view.behind
    if (view.status?.conflict_predicted === true) acc.conflicts += 1
    if (view.error) acc.errors += 1
    return acc
  }, { repos: 0, worktrees: 0, uncommitted: 0, push: 0, behind: 0, conflicts: 0, errors: 0 })
}

function pathKey(path: string, distro: string | null): string {
  const normalized = path.replace(/\\/g, '/').replace(/\/+$/, '')
  const host = isWslRepoPath(path) ? normalized : normalized.toLowerCase()
  return JSON.stringify([distro ?? null, host])
}

/**
 * Order rows so each linked worktree follows its main repository (marked
 * `nested`). Worktrees whose main repository is not in `views` stay in place.
 */
export function groupWorktrees(views: RepoViewModel[]): RepoViewModel[] {
  const present = new Set(views.map(view => pathKey(view.row.path, view.row.distro)))
  const children = new Map<string, RepoViewModel[]>()
  for (const view of views) {
    if (!view.worktreeOf) continue
    const parent = pathKey(view.worktreeOf, view.row.distro)
    if (!present.has(parent)) continue
    children.set(parent, [...(children.get(parent) ?? []), view])
  }
  const out: RepoViewModel[] = []
  for (const view of views) {
    if (view.worktreeOf && present.has(pathKey(view.worktreeOf, view.row.distro))) continue
    out.push({ ...view, nested: false })
    for (const child of children.get(pathKey(view.row.path, view.row.distro)) ?? []) {
      out.push({ ...child, nested: true })
    }
  }
  return out
}

export function FieldTip({
  tip,
  children,
  className = '',
  focusable = true,
}: {
  tip: string
  children: ReactNode
  className?: string
  focusable?: boolean
}) {
  const tooltipId = useId()
  const triggerRef = useRef<HTMLSpanElement>(null)
  const tooltipRef = useRef<HTMLSpanElement>(null)
  const [visible, setVisible] = useState(false)
  const [position, setPosition] = useState<{ left: number; top: number } | null>(null)

  useLayoutEffect(() => {
    if (!visible || typeof window === 'undefined') return
    const trigger = triggerRef.current
    const tooltip = tooltipRef.current
    if (!trigger || !tooltip) return

    const margin = 10
    const gap = 8
    const triggerRect = trigger.getBoundingClientRect()
    const tooltipRect = tooltip.getBoundingClientRect()
    const left = Math.min(
      window.innerWidth - tooltipRect.width - margin,
      Math.max(margin, triggerRect.left + triggerRect.width / 2 - tooltipRect.width / 2),
    )
    const topBelow = triggerRect.bottom + gap
    const topAbove = triggerRect.top - tooltipRect.height - gap
    const top = topBelow + tooltipRect.height + margin <= window.innerHeight
      ? topBelow
      : Math.max(margin, topAbove)

    setPosition({ left, top })
  }, [visible, tip])

  const show = () => {
    setPosition(null)
    setVisible(true)
  }
  const hide = () => setVisible(false)

  return (
    <span
      ref={triggerRef}
      className={`gc-tip ${className}`}
      tabIndex={focusable ? 0 : undefined}
      aria-describedby={tooltipId}
      onMouseEnter={show}
      onMouseLeave={hide}
      onFocus={show}
      onBlur={hide}
    >
      {children}
      <span
        ref={tooltipRef}
        id={tooltipId}
        role="tooltip"
        className={`gc-tip-text ${visible && position ? 'is-visible' : ''}`}
        style={position ?? undefined}
      >{tip}</span>
    </span>
  )
}

export function KpiCard({ label, value, sub, tone = 'muted', tip }: {
  label: string
  value: number | string
  sub: string
  tone?: StatusTone
  tip: string
}) {
  return (
    <FieldTip tip={tip} className={`gc-kpi tone-${tone}`}>
      <span className="gc-kpi-label">{label}</span>
      <span className="gc-kpi-value">{value}</span>
      <span className="gc-kpi-sub">{sub}</span>
    </FieldTip>
  )
}

function HostIcon({ host }: { host: HostKind }) {
  return host === 'wsl' ? <Server className="h-3.5 w-3.5" /> : <Monitor className="h-3.5 w-3.5" />
}

function StatusBarSegments({ view }: { view: RepoViewModel }) {
  if (view.error) return <span className="gc-clean-text tone-danger"><AlertTriangle className="h-3 w-3" /> error</span>

  const conflict = view.status?.conflict_predicted === true ? 1 : 0
  const total = view.uncommitted + view.push + view.behind + conflict
  if (total === 0) return <span className="gc-clean-text tone-ok"><Check className="h-3 w-3" /> clean</span>

  const width = (value: number) => `${Math.max(6, (value / total) * 100)}%`
  return (
    <FieldTip tip={`Uncommitted ${view.uncommitted}, Push ${view.push}, Behind ${view.behind}, Conflict ${conflict}`} className="gc-bar-tip" focusable={false}>
      <span className="gc-change-bar" aria-hidden="true">
        {view.uncommitted > 0 && <span className="gc-change-seg warn" style={{ width: width(view.uncommitted) }} />}
        {view.push > 0 && <span className="gc-change-seg info" style={{ width: width(view.push) }} />}
        {view.behind > 0 && <span className="gc-change-seg behind" style={{ width: width(view.behind) }} />}
        {conflict > 0 && <span className="gc-change-seg danger" style={{ width: width(conflict) }} />}
      </span>
    </FieldTip>
  )
}

export function RepoGridHeader({
  gridTemplateColumns,
  beginColumnResize,
}: {
  gridTemplateColumns: string
  beginColumnResize: (key: ColumnKey, event: ReactPointerEvent<HTMLButtonElement>) => void
}) {
  return (
    <div className="gc-grid-head" style={{ gridTemplateColumns }}>
      {TABLE_COLUMNS.map(column => (
        <div key={column.key} className={`gc-grid-cell ${column.align === 'center' ? 'center' : ''}`}>
          <FieldTip tip={column.tooltip} className="gc-head-tip">
            <span>{column.label}</span>
          </FieldTip>
          <button
            type="button"
            aria-label={`Resize ${column.label}`}
            onPointerDown={event => beginColumnResize(column.key, event)}
            className="gc-resize-handle"
          />
        </div>
      ))}
    </div>
  )
}

export function RepoListRow({ view, selected, onSelect, gridTemplateColumns }: {
  view: RepoViewModel
  selected: boolean
  onSelect: () => void
  gridTemplateColumns: string
}) {
  return (
    <button
      type="button"
      className={`gc-grid-row ${selected ? 'is-selected' : ''} ${view.nested ? 'is-worktree' : ''}`}
      data-state={view.state}
      onClick={onSelect}
      style={{ gridTemplateColumns }}
      aria-label={`${view.displayName} details`}
    >
      <div className="gc-grid-cell repo">
        <FieldTip tip={`Repository path: ${view.repository}`} className="gc-repo-tip" focusable={false}>
          <span className="gc-host-icon"><HostIcon host={view.host} /></span>
          <span className={`gc-pip tone-${view.tone}`} />
          <span className="gc-repo-text">
            {view.worktreeOf && (
              <span className="gc-worktree-badge" title={`Linked worktree of ${view.worktreeOf}`}>
                <GitFork className="h-3 w-3" /> worktree
              </span>
            )}
            <span className="gc-repo-name">{view.displayName}</span>
            <span className="gc-repo-path"> {view.parentPath}</span>
          </span>
        </FieldTip>
      </div>
      <div className="gc-grid-cell branch">
        <FieldTip tip={view.error ? `Scan error: ${view.error}` : `Current branch: ${view.branch}`} focusable={false}>
          {view.error ? (
            <span className="gc-error-inline"><AlertTriangle className="h-3 w-3" /> {view.error}</span>
          ) : (
            <span className="gc-branch"><GitBranch className="h-3 w-3" /> {view.branch}</span>
          )}
        </FieldTip>
      </div>
      <div className="gc-grid-cell center">
        <FieldTip tip={`${view.uncommitted} files have uncommitted changes.`} focusable={false}>
          <span className={view.uncommitted > 0 ? 'gc-number tone-warn' : 'gc-number'}>{view.uncommitted}</span>
        </FieldTip>
      </div>
      <div className="gc-grid-cell center">
        <FieldTip tip={`${view.push} local branches need push.`} focusable={false}>
          <span className={view.push > 0 ? 'gc-number tone-info' : 'gc-number'}>{view.push}</span>
        </FieldTip>
      </div>
      <div className="gc-grid-cell center">
        <FieldTip tip={`${view.ahead} commits ahead, ${view.behind} commits behind upstream.`} focusable={false}>
          <span className="gc-sync"><ArrowUp className="h-3 w-3" />{view.ahead}<span>/</span><ArrowDown className="h-3 w-3" />{view.behind}</span>
        </FieldTip>
      </div>
      <div className="gc-grid-cell center">
        <FieldTip tip={view.conflictTooltip} focusable={false}>
          <span className={view.status?.conflict_predicted === true ? 'gc-number tone-danger' : 'gc-number'}>{view.conflictLabel}</span>
        </FieldTip>
      </div>
      <div className="gc-grid-cell center">
        <FieldTip tip={view.issues === null ? 'GitHub issue count is unavailable.' : `${view.issues} open GitHub issues.`} focusable={false}>
          <span className="gc-number">{view.issues ?? '-'}</span>
        </FieldTip>
      </div>
      <div className="gc-grid-cell center">
        <FieldTip tip={view.prs === null ? 'GitHub pull request count is unavailable.' : `${view.prs} open GitHub pull requests.`} focusable={false}>
          <span className="gc-number">{view.prs ?? '-'}</span>
        </FieldTip>
      </div>
      <div className="gc-grid-cell center">
        <FieldTip tip={`Last scan time: ${view.lastScan}`} focusable={false}>
          <span className="gc-time"><Clock className="h-3 w-3" /> {view.lastScan}</span>
        </FieldTip>
      </div>
      <div className="gc-row-status">
        {view.status?.fetch_warning ? (
          <span className="gc-fetch-warning" title={view.status.fetch_warning}><AlertTriangle className="h-3 w-3" /> {view.status.fetch_warning}</span>
        ) : <StatusBarSegments view={view} />}
      </div>
    </button>
  )
}

export function DetailPanel({
  view,
  onClose,
  onOpenFolder,
  onOpenGithub,
  onRemove,
}: {
  view: RepoViewModel
  onClose: () => void
  onOpenFolder: (view: RepoViewModel) => void
  onOpenGithub: (view: RepoViewModel) => void
  onRemove?: (view: RepoViewModel) => void
}) {
  const recentCommits = view.status?.recent_commits ?? []

  return (
    <aside className="gc-detail" aria-label="Repository detail">
      <div className="gc-detail-head">
        <div className="gc-detail-title">
          <span className="gc-host-icon"><HostIcon host={view.host} /></span>
          <span>{view.displayName}</span>
        </div>
        <Button variant="ghost" size="icon" className="gc-close-btn" onClick={onClose} aria-label="Close detail pane">
          <X className="h-4 w-4" />
        </Button>
      </div>
      <FieldTip tip="Full path of the monitored repository." className="gc-detail-path">
        {view.repository.replace(/^\*\s*/, '')}
      </FieldTip>
      <div className="gc-detail-actions" aria-label="Repository actions">
        <Button variant="outline" size="sm" onClick={() => onOpenFolder(view)} disabled={view.notFound}>
          <FolderOpen className="h-3.5 w-3.5" />
          {view.host === 'wsl' ? 'Open WSL folder' : 'Open in Explorer'}
        </Button>
        <Button variant="outline" size="sm" onClick={() => onOpenGithub(view)} disabled={!view.githubUrl}>
          <ExternalLink className="h-3.5 w-3.5" />
          GitHub
        </Button>
        {onRemove && (
          <Button
            variant={view.notFound ? 'destructive' : 'outline'}
            size="sm"
            onClick={() => onRemove(view)}
            title="Remove from the list. Files on disk are not touched."
          >
            <Trash2 className="h-3.5 w-3.5" />
            Remove
          </Button>
        )}
      </div>

      {view.error ? (
        <section className="gc-detail-error">
          <h2><AlertTriangle className="h-4 w-4" /> {view.notFound ? 'Path not found' : 'Scan error'}</h2>
          <p>{view.error}</p>
        </section>
      ) : (
        <>
          <section className="gc-detail-section">
            <h2>Working tree</h2>
            <div className="gc-stat-grid">
              <DetailStat label="Uncommitted" value={view.uncommitted} tone="warn" tip="Number of files with uncommitted changes." />
              <DetailStat label="To push" value={view.push} tone="info" tip="Number of branches that need to be pushed." />
              <DetailStat label="Behind" value={view.behind} tone="info" tip="Number of commits behind upstream." />
              <DetailStat label="Conflict" value={view.conflictLabel} tone={view.status?.conflict_predicted === true ? 'danger' : 'muted'} tip={view.conflictTooltip} />
            </div>
          </section>

          <section className="gc-detail-section">
            <h2>Branch & sync</h2>
            <div className="gc-detail-line"><GitBranch className="h-3.5 w-3.5" /> {view.branch}</div>
            <div className="gc-detail-line"><ArrowUp className="h-3.5 w-3.5" /> {view.ahead} ahead <span>·</span> <ArrowDown className="h-3.5 w-3.5" /> {view.behind} behind</div>
          </section>

          <section className="gc-detail-section">
            <h2>GitHub</h2>
            <div className="gc-detail-line"><GitBranch className="h-3.5 w-3.5" /> Issues {view.issues ?? '-'} <span>·</span> PRs {view.prs ?? '-'}</div>
          </section>

          <section className="gc-detail-section">
            <div className="gc-section-title-row">
              <h2>Recent commits</h2>
            </div>
            <div className="gc-commits">
              {recentCommits.length === 0 ? (
                <p className="gc-detail-note">No recent commits found.</p>
              ) : (
                recentCommits.map(commit => (
                  <FieldTip key={`${commit.short_hash}-${commit.committed_at}`} tip={`${commit.author_name} · ${formatCommitTime(commit.committed_at)}`} className="gc-commit-row">
                    <span>{commit.short_hash}</span>
                    <span>·</span>
                    <span>{commit.summary}</span>
                    <span>·</span>
                    <span>{commit.author_name}</span>
                    <span>·</span>
                    <span>{formatCommitTime(commit.committed_at)}</span>
                  </FieldTip>
                ))
              )}
            </div>
          </section>
        </>
      )}
    </aside>
  )
}

function DetailStat({ label, value, tone, tip }: { label: string; value: number | string; tone: StatusTone; tip: string }) {
  return (
    <FieldTip tip={tip} className={`gc-detail-stat tone-${tone}`}>
      <span className="gc-detail-stat-value">{value}</span>
      <span className="gc-detail-stat-label">{label}</span>
    </FieldTip>
  )
}

export function AlertBanner({ tone, children, onClose }: { tone: 'warn' | 'danger'; children: ReactNode; onClose: () => void }) {
  return (
    <div className={`gc-alert tone-${tone}`}>
      <span>{children}</span>
      <button type="button" onClick={onClose}>Close</button>
    </div>
  )
}
