import { useEffect, useMemo, useRef, useState } from 'react'
import { getConfig } from '@/api'
import type { AppConfigDto, CommandError } from '@/types'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { GeneralTab } from './tabs/GeneralTab'
import { RepositoriesTab } from './tabs/RepositoriesTab'
import { ScanRootsTab } from './tabs/ScanRootsTab'
import { GithubTab } from './tabs/GithubTab'
import { NotificationsTab } from './tabs/NotificationsTab'
import { ShortcutTab } from './tabs/ShortcutTab'
import { ThemeTab } from './tabs/ThemeTab'
import { useDebouncedSave } from './useDebouncedSave'
import { hasErrors, validateConfig } from './settingsValidation'

interface Props {
  open: boolean
  onOpenChange: (open: boolean) => void
  onConfigChange?: (config: AppConfigDto) => void
}

type DraftChange = AppConfigDto | ((current: AppConfigDto) => AppConfigDto)

const TEXT_DELAY_MS = 400

export function SettingsDialog({ open, onOpenChange, onConfigChange }: Props) {
  const [draft, setDraft] = useState<AppConfigDto | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [immediate, setImmediate] = useState(false)
  const draftRef = useRef<AppConfigDto | null>(null)

  const resetDraft = () => {
    draftRef.current = null
    setDraft(null)
    setLoading(false)
    setImmediate(false)
  }

  const closing = useRef(false)
  const handleOpenChange = async (nextOpen: boolean) => {
    if (nextOpen) { onOpenChange(true); return }
    if (closing.current) return
    const current = draftRef.current
    if (current != null) {
      closing.current = true
      const saved = await status.flush(current)
      closing.current = false
      // Keep errors and the draft visible; close retries the same queue.
      if (!saved || draftRef.current !== current) return
      onConfigChange?.(current)
    }
    resetDraft()
    onOpenChange(false)
  }

  // Reload config on every open; do not carry over the previous draft/baseline.
  useEffect(() => {
    let cancelled = false
    if (open) {
      resetDraft()
      setLoadError(null)
      setLoading(true)
      getConfig()
        .then(cfg => {
          if (!cancelled) {
            draftRef.current = cfg
            setDraft(cfg)
            onConfigChange?.(cfg)
          }
        })
        .catch((e: unknown) => {
          if (cancelled) return
          const err = e as Partial<CommandError>
          setLoadError(err.message ?? String(e))
        })
        .finally(() => {
          if (!cancelled) {
            setLoading(false)
          }
        })
    }
    return () => {
      cancelled = true
    }
  }, [onConfigChange, open])

  const status = useDebouncedSave(draft, {
    textDelayMs: TEXT_DELAY_MS,
    immediate,
    savedVisibleMs: 3000,
  })

  const errors = useMemo(() => (draft ? validateConfig(draft) : {}), [draft])

  const onChange = (change: DraftChange, isImmediate: boolean) => {
    const current = draftRef.current
    if (current == null) return
    const next = typeof change === 'function' ? change(current) : change
    draftRef.current = next
    setImmediate(isImmediate)
    setDraft(next)
    onConfigChange?.(next)
  }

  const tabHasError = (prefix: string) =>
    Object.keys(errors).some(k => k.startsWith(prefix))

  const statusBadge = (() => {
    switch (status.status) {
      case 'saving':
        return <span className="text-xs text-gray-500 dark:text-gray-400">Saving...</span>
      case 'saved':
        return <span className="text-xs text-green-600">Saved</span>
      case 'error':
        return (
          <span className="text-xs text-red-600">
            Save failed [{status.kind}]: {status.message}
          </span>
        )
      default:
        if (hasErrors(errors)) {
          return <span className="text-xs text-red-600">Fix validation errors</span>
        }
        return null
    }
  })()

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="gc-settings-dialog">
        <DialogHeader className="gc-settings-header">
          <div className="gc-settings-title-row">
            <DialogTitle>Settings</DialogTitle>
            <div className="gc-settings-status">{statusBadge}</div>
          </div>
          <DialogDescription>
            Default config: <code>%APPDATA%\git-clawler\config.toml</code>
            {' '}(overridden by XDG_CONFIG_HOME)
          </DialogDescription>
        </DialogHeader>

        {loadError && (
          <div className="gc-settings-error">
            Failed to load settings: {loadError}
          </div>
        )}

        {loading && !loadError && (
          <div className="gc-settings-loading" role="status" aria-live="polite">
            Loading settings...
          </div>
        )}

        {!loading && draft && (
          <Tabs defaultValue="general" orientation="vertical" className="gc-settings-tabs-root">
            <TabsList className="gc-settings-tabs">
              <TabsTrigger value="general" className="gc-settings-tab">
                General
                {tabHasError('general.') && <span className="text-red-600">●</span>}
              </TabsTrigger>
              <TabsTrigger value="repositories" className="gc-settings-tab">
                Repositories
                {tabHasError('repositories[') && <span className="text-red-600">●</span>}
              </TabsTrigger>
              <TabsTrigger value="scan_roots" className="gc-settings-tab">
                Scan Roots
                {tabHasError('scan_roots[') && <span className="text-red-600">●</span>}
              </TabsTrigger>
              <TabsTrigger value="github" className="gc-settings-tab">
                GitHub
                {tabHasError('github.') && <span className="text-red-600">●</span>}
              </TabsTrigger>
              <TabsTrigger value="notifications" className="gc-settings-tab">
                Notifications
                {tabHasError('notifications.') && <span className="text-red-600">●</span>}
              </TabsTrigger>
              <TabsTrigger value="theme" className="gc-settings-tab">
                Theme
                {tabHasError('gui.theme') && <span className="text-red-600">●</span>}
              </TabsTrigger>
              <TabsTrigger value="shortcut" className="gc-settings-tab">
                Shortcut
                {tabHasError('gui.global_shortcut') && <span className="text-red-600">●</span>}
              </TabsTrigger>
            </TabsList>
            <TabsContent value="general" className="gc-settings-content">
              <GeneralTab draft={draft} onChange={onChange} errors={errors} />
            </TabsContent>
            <TabsContent value="repositories" className="gc-settings-content">
              <RepositoriesTab draft={draft} onChange={onChange} errors={errors} />
            </TabsContent>
            <TabsContent value="scan_roots" className="gc-settings-content">
              <ScanRootsTab draft={draft} onChange={onChange} errors={errors} />
            </TabsContent>
            <TabsContent value="github" className="gc-settings-content">
              <GithubTab draft={draft} onChange={onChange} errors={errors} />
            </TabsContent>
            <TabsContent value="notifications" className="gc-settings-content">
              <NotificationsTab draft={draft} onChange={onChange} errors={errors} />
            </TabsContent>
            <TabsContent value="theme" className="gc-settings-content">
              <ThemeTab draft={draft} onChange={onChange} errors={errors} />
            </TabsContent>
            <TabsContent value="shortcut" className="gc-settings-content">
              <ShortcutTab draft={draft} onChange={onChange} errors={errors} />
            </TabsContent>
          </Tabs>
        )}
      </DialogContent>
    </Dialog>
  )
}
