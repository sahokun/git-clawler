import appIcon from './assets/app-icon.png'
import { useState, useEffect, useCallback, useRef, useMemo } from 'react'
import type { PointerEvent as ReactPointerEvent } from 'react'
import { Clock, Filter, GitBranch, Monitor, RefreshCw, Search, Server, Settings } from 'lucide-react'
import {
  getConfig,
  getStartupErrors,
  listRepositories,
  openGithubPage,
  openRepositoryFolder,
  requestRefresh,
  saveColumnWidths,
  subscribeRepositories,
  subscribeRepositoriesError,
  subscribeShortcutStatus,
  subscribeSettingsOpen,
  removeRepository,
} from './api'
import type { AppConfigDto, ListRepositoriesResponse, RealtimeErrorPayload } from './types'
import { SettingsDialog } from './settings/SettingsDialog'
import { Button } from './components/ui/button'
import {
  AlertBanner,
  DetailPanel,
  FieldTip,
  KpiCard,
  RepoGridHeader,
  RepoListRow,
  TABLE_COLUMNS,
  hasChanges,
  matchesSearch,
  repoKey,
  groupWorktrees,
  summarize,
  toViewModel,
} from './hybridUi'
import type { ColumnKey } from './hybridUi'

type State =
  | { status: 'loading' }
  | { status: 'error'; error: string }
  | { status: 'ready'; data: ListRepositoriesResponse }

type HostFilter = 'all' | 'native' | 'wsl'
type GroupMode = 'host' | 'none'

interface DragState {
  key: ColumnKey
  startX: number
  startWidth: number
}

const MIN_COLUMN_WIDTH = 40
const MAX_COLUMN_WIDTH = 1600

function applyTheme(theme: AppConfigDto['gui']['theme']) {
  document.documentElement.classList.toggle('dark', theme === 'dark')
  document.documentElement.style.colorScheme = theme
}

function clampColumnWidth(width: number): number {
  return Math.min(MAX_COLUMN_WIDTH, Math.max(MIN_COLUMN_WIDTH, Math.round(width)))
}

function mergeColumnWidths(stored: Record<string, number> | undefined): Record<ColumnKey, number> {
  return TABLE_COLUMNS.reduce<Record<ColumnKey, number>>((acc, column) => {
    const value = stored?.[column.key]
    acc[column.key] = clampColumnWidth(
      typeof value === 'number' && Number.isFinite(value) ? value : column.defaultWidth,
    )
    return acc
  }, {} as Record<ColumnKey, number>)
}

function persistedColumnWidths(widths: Record<ColumnKey, number>): Record<string, number> {
  return TABLE_COLUMNS.reduce<Record<string, number>>(
    (acc, column) => {
      acc[column.key] = widths[column.key]
      return acc
    },
    {},
  )
}

function errorMessage(e: unknown): string {
  return typeof e === 'object' && e !== null && 'message' in e
    ? String((e as { message: unknown }).message)
    : String(e)
}

function App() {
  const [state, setState] = useState<State>({ status: 'loading' })
  // A manual refresh keeps the current rows on screen until new ones arrive.
  const [refreshing, setRefreshing] = useState(false)
  const [watcherError, setWatcherError] = useState<RealtimeErrorPayload | null>(null)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [guiConfig, setGuiConfig] = useState<AppConfigDto | null>(null)
  const [columnWidths, setColumnWidths] = useState<Record<ColumnKey, number>>(() => mergeColumnWidths(undefined))
  const [preferenceError, setPreferenceError] = useState<string | null>(null)
  const [hostFilter, setHostFilter] = useState<HostFilter>('all')
  const [query, setQuery] = useState('')
  const [changesOnly, setChangesOnly] = useState(false)
  const [groupMode, setGroupMode] = useState<GroupMode>('host')
  const [selectedKey, setSelectedKey] = useState<string | null>(null)
  const [detailOpen, setDetailOpen] = useState(true)
  const dragRef = useRef<DragState | null>(null)
  const pendingWidthsRef = useRef<Record<ColumnKey, number>>(columnWidths)
  const shortcutStatusSeenRef = useRef(false)

  const handleConfigChange = useCallback((next: AppConfigDto) => {
    setGuiConfig(next)
    setColumnWidths(mergeColumnWidths(next.gui.column_widths))
    applyTheme(next.gui.theme)
  }, [])

  const loadGuiConfig = useCallback(async () => {
    applyTheme('dark')
    try {
      const config = await getConfig()
      handleConfigChange(config)
      setPreferenceError(null)
    } catch (e) {
      setPreferenceError(errorMessage(e))
    }
  }, [handleConfigChange])

  const persistColumnWidths = useCallback(async (widths: Record<ColumnKey, number>) => {
    try {
      const next = await saveColumnWidths(persistedColumnWidths(widths))
      handleConfigChange(next)
      setPreferenceError(null)
    } catch (e) {
      setPreferenceError(errorMessage(e))
    }
  }, [handleConfigChange])

  const fetchRepos = useCallback(async () => {
    setState({ status: 'loading' })
    try {
      const data = await listRepositories()
      setState({ status: 'ready', data })
      setSelectedKey(previous => previous ?? (data.rows[0] ? repoKey(data.rows[0]) : null))
    } catch (e) {
      setState({ status: 'error', error: errorMessage(e) })
    }
  }, [])

  const refreshRepos = useCallback(async () => {
    setState(previous => (previous.status === 'ready' ? previous : { status: 'loading' }))
    setRefreshing(true)
    try {
      await requestRefresh()
    } catch (e) {
      setRefreshing(false)
      setWatcherError({
        kind: 'refresh',
        message: errorMessage(e),
        occurred_at: new Date().toISOString(),
      })
      setState(previous => (
        previous.status === 'loading'
          ? { status: 'error', error: errorMessage(e) }
          : previous
      ))
    }
  }, [])

  useEffect(() => {
    void loadGuiConfig()
    void fetchRepos()
  }, [fetchRepos, loadGuiConfig])

  useEffect(() => {
    const onPointerMove = (event: PointerEvent) => {
      const drag = dragRef.current
      if (!drag) return
      const width = clampColumnWidth(drag.startWidth + event.clientX - drag.startX)
      setColumnWidths(previous => {
        const next = { ...previous, [drag.key]: width }
        pendingWidthsRef.current = next
        return next
      })
    }

    const onPointerUp = () => {
      if (!dragRef.current) return
      dragRef.current = null
      void persistColumnWidths(pendingWidthsRef.current)
    }

    window.addEventListener('pointermove', onPointerMove)
    window.addEventListener('pointerup', onPointerUp)
    return () => {
      window.removeEventListener('pointermove', onPointerMove)
      window.removeEventListener('pointerup', onPointerUp)
    }
  }, [persistColumnWidths])

  useEffect(() => {
    let unlistenRepos: (() => void) | null = null
    let unlistenError: (() => void) | null = null
    let unlistenShortcutStatus: (() => void) | null = null
    let cancelled = false

    subscribeRepositories(e => {
      setState({ status: 'ready', data: e.payload })
      setRefreshing(false)
      setWatcherError(previous => previous?.kind === 'shortcut' ? previous : null)
      setSelectedKey(previous => previous ?? (e.payload.rows[0] ? repoKey(e.payload.rows[0]) : null))
    })
      .then(fn => {
        if (cancelled) fn()
        else unlistenRepos = fn
      })
      .catch(e => {
        if (!cancelled) {
          setWatcherError({
            kind: 'frontend',
            message: `repositories subscription failed: ${errorMessage(e)}`,
            occurred_at: new Date().toISOString(),
          })
        }
      })

    subscribeRepositoriesError(e => {
      setWatcherError(e.payload)
      setRefreshing(false)
      setState(previous => (
        previous.status === 'loading'
          ? { status: 'error', error: e.payload.message }
          : previous
      ))
    })
      .then(fn => {
        if (cancelled) fn()
        else unlistenError = fn
      })
      .catch(e => {
        if (!cancelled) {
          setWatcherError({
            kind: 'frontend',
            message: `error subscription failed: ${errorMessage(e)}`,
            occurred_at: new Date().toISOString(),
          })
        }
      })

    const loadStartupErrors = (onEmpty?: () => void) => {
      getStartupErrors()
        .then(errors => {
          if (cancelled) return
          if (errors.length === 0) {
            onEmpty?.()
            return
          }
          const latest = errors[errors.length - 1]
          setWatcherError(previous => {
            if (previous != null) return previous
            if (latest.kind === 'shortcut' && shortcutStatusSeenRef.current) return null
            return latest
          })
        })
        .catch(e => {
          if (!cancelled) {
            setWatcherError({
              kind: 'frontend',
              message: `startup error load failed: ${errorMessage(e)}`,
              occurred_at: new Date().toISOString(),
            })
          }
        })
    }

    subscribeShortcutStatus(() => {
      shortcutStatusSeenRef.current = true
      setWatcherError(previous => previous?.kind === 'shortcut' ? null : previous)
    })
      .then(fn => {
        if (cancelled) fn()
        else {
          unlistenShortcutStatus = fn
          loadStartupErrors()
        }
      })
      .catch(e => {
        if (!cancelled) {
          const subscriptionError: RealtimeErrorPayload = {
            kind: 'frontend',
            message: `shortcut status subscription failed: ${errorMessage(e)}`,
            occurred_at: new Date().toISOString(),
          }
          loadStartupErrors(() => {
            setWatcherError(previous => previous ?? subscriptionError)
          })
        }
      })

    return () => {
      cancelled = true
      unlistenRepos?.()
      unlistenError?.()
      unlistenShortcutStatus?.()
    }
  }, [])

  useEffect(() => {
    let unlisten: (() => void) | null = null
    let cancelled = false

    subscribeSettingsOpen(() => {
      setSettingsOpen(true)
    })
      .then(fn => {
        if (cancelled) fn()
        else unlisten = fn
      })
      .catch(e => {
        if (!cancelled) setPreferenceError(`settings subscription failed: ${errorMessage(e)}`)
      })

    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])

  const beginColumnResize = (key: ColumnKey, event: ReactPointerEvent<HTMLButtonElement>) => {
    event.preventDefault()
    dragRef.current = { key, startX: event.clientX, startWidth: columnWidths[key] }
    pendingWidthsRef.current = columnWidths
  }

  const busy = state.status === 'loading' || refreshing
  const allViews = useMemo(() => (state.status === 'ready' ? state.data.rows.map(toViewModel) : []), [state])

  const filteredViews = useMemo(() => allViews.filter(view => {
    if (hostFilter !== 'all' && view.host !== hostFilter) return false
    if (changesOnly && !hasChanges(view)) return false
    return matchesSearch(view, query)
  }), [allViews, changesOnly, hostFilter, query])

  useEffect(() => {
    if (filteredViews.length === 0) {
      if (selectedKey !== null) setSelectedKey(null)
      if (detailOpen) setDetailOpen(false)
      return
    }

    if (selectedKey === null) {
      setSelectedKey(repoKey(filteredViews[0].row))
      return
    }

    if (!filteredViews.some(view => repoKey(view.row) === selectedKey)) {
      setSelectedKey(null)
      setDetailOpen(false)
    }
  }, [detailOpen, filteredViews, selectedKey])

  const totals = useMemo(() => summarize(allViews), [allViews])
  const visibleTotals = useMemo(() => summarize(filteredViews), [filteredViews])
  const selectedView = detailOpen && selectedKey !== null
    ? filteredViews.find(view => repoKey(view.row) === selectedKey) ?? null
    : null

  const gridTemplateColumns = TABLE_COLUMNS.map(column => (
    `${columnWidths[column.key]}px`
  )).join(' ')
  const minTableWidth = TABLE_COLUMNS.reduce((sum, column) => sum + columnWidths[column.key], 0)

  const groups = useMemo(() => {
    if (groupMode === 'none') return [{ key: 'all', label: null as string | null, views: groupWorktrees(filteredViews) }]
    return [
      { key: 'native', label: 'Native', views: groupWorktrees(filteredViews.filter(view => view.host === 'native')) },
      { key: 'wsl', label: 'WSL', views: groupWorktrees(filteredViews.filter(view => view.host === 'wsl')) },
    ].filter(group => group.views.length > 0)
  }, [filteredViews, groupMode])

  const refreshInterval = guiConfig?.general.refresh_interval_secs ?? 30
  const lastRefresh = allViews[0]?.lastScan ?? '-'

  const handleSelectView = (key: string) => {
    setSelectedKey(key)
    setDetailOpen(true)
  }

  const handleOpenFolder = async (view: ReturnType<typeof toViewModel>) => {
    try {
      await openRepositoryFolder(view.row.path, view.row.distro)
      setPreferenceError(null)
    } catch (e) {
      setPreferenceError(errorMessage(e))
    }
  }

  const handleRemove = async (view: ReturnType<typeof toViewModel>) => {
    try {
      await removeRepository(view.row.path, view.row.distro)
      // Drop the row now; the rescan triggered by the config change reconciles.
      const removedKey = repoKey(view.row)
      setState(previous => (
        previous.status === 'ready'
          ? { status: 'ready', data: { ...previous.data, rows: previous.data.rows.filter(row => repoKey(row) !== removedKey) } }
          : previous
      ))
      setDetailOpen(false)
      setPreferenceError(null)
    } catch (e) {
      setPreferenceError(errorMessage(e))
    }
  }

  const handleOpenGithub = async (view: ReturnType<typeof toViewModel>) => {
    if (!view.remoteUrl) {
      setPreferenceError('No GitHub remote URL is available for this repository.')
      return
    }
    try {
      await openGithubPage(view.remoteUrl)
      setPreferenceError(null)
    } catch (e) {
      setPreferenceError(errorMessage(e))
    }
  }

  return (
    <div className="gc-app-shell">
      <SettingsDialog open={settingsOpen} onOpenChange={setSettingsOpen} onConfigChange={handleConfigChange} />

      <header className="gc-header">
        <div className="gc-brand">
          <img className="gc-brand-mark" src={appIcon} alt="" aria-hidden="true" />
          <div className="gc-brand-chip" aria-label="Workspace summary">
            <GitBranch className="h-3.5 w-3.5" />
            <span>{totals.repos} repos{totals.worktrees > 0 ? ` (+${totals.worktrees} worktrees)` : ''}</span>
            <span className="gc-chip-separator">·</span>
            <span>{guiConfig?.scan_roots.length ?? 0} roots</span>
          </div>
        </div>
        <div className="gc-header-actions">
          <FieldTip tip={`Auto-refresh interval is ${refreshInterval} seconds.`} className="gc-auto-chip">
            <Clock className="h-3.5 w-3.5" /> auto · {refreshInterval}s <span className="gc-live-dot" />
          </FieldTip>
          <Button onClick={refreshRepos} disabled={busy} className="gc-primary-btn">
            <RefreshCw className={`h-4 w-4 ${busy ? 'animate-spin' : ''}`} />
            {busy ? 'Loading...' : 'Refresh now'}
          </Button>
          <Button variant="ghost" size="icon" onClick={() => setSettingsOpen(true)} aria-label="Settings">
            <Settings className="h-5 w-5" />
          </Button>
        </div>
      </header>

      {preferenceError && (
        <AlertBanner tone="warn" onClose={() => setPreferenceError(null)}>
          Failed to apply settings: {preferenceError}
        </AlertBanner>
      )}

      {watcherError && (
        <AlertBanner tone="danger" onClose={() => setWatcherError(null)}>
          <strong>[{watcherError.kind}]</strong> {watcherError.message} <span>{watcherError.occurred_at}</span>
        </AlertBanner>
      )}

      <section className="gc-kpis" aria-label="Repository summary">
        <KpiCard label="Repositories" value={visibleTotals.repos} sub={`${totals.repos} tracked`} tip="Repositories currently visible after filters are applied." />
        <KpiCard label="Uncommitted" value={visibleTotals.uncommitted} sub="files waiting" tone={visibleTotals.uncommitted ? 'warn' : 'ok'} tip="Total uncommitted files in the visible repositories." />
        <KpiCard label="To Push" value={visibleTotals.push} sub="branches" tone={visibleTotals.push ? 'info' : 'ok'} tip="Total branches that need to be pushed in the visible repositories." />
        <KpiCard label="Behind" value={visibleTotals.behind} sub="commits" tone={visibleTotals.behind ? 'info' : 'ok'} tip="Total commits behind upstream in the visible repositories." />
        <KpiCard label="Conflicts" value={visibleTotals.conflicts} sub={visibleTotals.conflicts ? 'attention' : 'clean'} tone={visibleTotals.conflicts ? 'danger' : 'ok'} tip="Repositories with predicted pull conflicts." />
        <KpiCard label="Errors" value={visibleTotals.errors} sub={visibleTotals.errors ? 'scan failed' : 'all green'} tone={visibleTotals.errors ? 'danger' : 'ok'} tip="Repositories whose latest scan failed." />
      </section>

      <section className="gc-toolbar" aria-label="Repository filters">
        <div className="gc-segmented" role="group" aria-label="Host filter">
          <button type="button" aria-pressed={hostFilter === 'all'} onClick={() => setHostFilter('all')}>All <span>{allViews.length}</span></button>
          <button type="button" aria-pressed={hostFilter === 'native'} onClick={() => setHostFilter('native')}><Monitor className="h-3 w-3" /> Native <span>{allViews.filter(view => view.host === 'native').length}</span></button>
          <button type="button" aria-pressed={hostFilter === 'wsl'} onClick={() => setHostFilter('wsl')}><Server className="h-3 w-3" /> WSL <span>{allViews.filter(view => view.host === 'wsl').length}</span></button>
        </div>
        <label className="gc-search">
          <Search className="h-4 w-4" />
          <input value={query} onChange={event => setQuery(event.target.value)} placeholder="filter… has:conflict branch:main host:wsl" />
          <span>/</span>
        </label>
        <button type="button" className={`gc-toggle ${changesOnly ? 'active' : ''}`} onClick={() => setChangesOnly(value => !value)}>
          <Filter className="h-3.5 w-3.5" /> Changes only
        </button>
        <div className="gc-group-control">
          <span>Group</span>
          <div className="gc-segmented compact" role="group" aria-label="Group mode">
            <button type="button" aria-pressed={groupMode === 'host'} onClick={() => setGroupMode('host')}>Host</button>
            <button type="button" aria-pressed={groupMode === 'none'} onClick={() => setGroupMode('none')}>None</button>
          </div>
        </div>
      </section>

      <main className="gc-body">
        <section className="gc-list-pane" aria-label="Repositories">
          {state.status === 'loading' && <p className="gc-empty">Loading repositories...</p>}
          {state.status === 'error' && <div className="gc-error-box">Error: {state.error}</div>}
          {state.status === 'ready' && state.data.rows.length === 0 && <p className="gc-empty">No repositories registered.</p>}
          {state.status === 'ready' && state.data.rows.length > 0 && filteredViews.length === 0 && (
            <div className="gc-empty-box">
              <Search className="h-5 w-5" />
              <p>No repositories match.</p>
              <button type="button" onClick={() => { setQuery(''); setChangesOnly(false); setHostFilter('all') }}>Reset filters</button>
            </div>
          )}
          {state.status === 'ready' && filteredViews.length > 0 && (
            <div className="gc-list-scroll" style={{ minWidth: minTableWidth }}>
              {groups.map(group => (
                <section className="gc-group" key={group.key}>
                  {group.label && (
                    <div className="gc-group-head">
                      {group.key === 'wsl' ? <Server className="h-3.5 w-3.5" /> : <Monitor className="h-3.5 w-3.5" />}
                      <span>{group.label}</span>
                      <strong>{group.views.length}</strong>
                    </div>
                  )}
                  <div className="gc-grid-wrap">
                    <RepoGridHeader gridTemplateColumns={gridTemplateColumns} beginColumnResize={beginColumnResize} />
                    {group.views.map(view => (
                      <RepoListRow
                        key={repoKey(view.row)}
                        view={view}
                        selected={selectedView ? repoKey(selectedView.row) === repoKey(view.row) : false}
                        onSelect={() => handleSelectView(repoKey(view.row))}
                        gridTemplateColumns={gridTemplateColumns}
                      />
                    ))}
                  </div>
                </section>
              ))}
            </div>
          )}
        </section>

        {selectedView && (
          <DetailPanel
            view={selectedView}
            onClose={() => setDetailOpen(false)}
            onOpenFolder={handleOpenFolder}
            onOpenGithub={handleOpenGithub}
            onRemove={handleRemove}
          />
        )}
      </main>

      <footer className="gc-statusbar">
        <span><span className="gc-pip tone-ok" /> ready</span>
        <span>·</span>
        <span>{totals.repos} repos · {visibleTotals.errors ? `${visibleTotals.errors} errors` : 'no errors'}</span>
        <span className="gc-status-spacer" />
        <span>last refresh {lastRefresh}</span>
        <span>·</span>
        <span>{guiConfig?.gui.theme ?? 'dark'} theme</span>
      </footer>
    </div>
  )
}

export default App
