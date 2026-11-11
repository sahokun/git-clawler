import { useCallback, useEffect, useRef, useState } from 'react'
import { saveConfig } from '@/api'
import type { AppConfigDto, CommandError } from '@/types'
import { hasErrors, validateConfig } from './settingsValidation'

export type SaveStatus =
  | { status: 'idle' }
  | { status: 'saving' }
  | { status: 'saved' }
  | { status: 'error'; kind: string; message: string }

export interface UseDebouncedSaveOptions {
  textDelayMs: number
  immediate: boolean
  savedVisibleMs?: number
}

/** Autosave and close share one queue: an older write cannot overwrite a newer draft. */
export function useDebouncedSave(
  draft: AppConfigDto | null,
  options: UseDebouncedSaveOptions,
): SaveStatus & { flush: (value: AppConfigDto) => Promise<boolean> } {
  const [status, setStatus] = useState<SaveStatus>({ status: 'idle' })
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const savedTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const baseline = useRef<string | null>(null)
  const current = useRef(draft)
  current.current = draft
  const queue = useRef<Promise<unknown>>(Promise.resolve())
  const generation = useRef(0)
  const visibleMs = useRef(options.savedVisibleMs ?? 3000)
  visibleMs.current = options.savedVisibleMs ?? 3000

  const flush = useCallback((value: AppConfigDto): Promise<boolean> => {
    if (timer.current != null) clearTimeout(timer.current)
    timer.current = null
    if (hasErrors(validateConfig(value))) return Promise.resolve(false)
    const json = JSON.stringify(value)
    const epoch = generation.current
    const isCurrent = () => epoch === generation.current && JSON.stringify(current.current) === json
    const operation = queue.current.then(async () => {
      if (epoch !== generation.current) return false
      if (baseline.current === json) return true
      if (savedTimer.current != null) clearTimeout(savedTimer.current)
      if (isCurrent()) setStatus({ status: 'saving' })
      try {
        await saveConfig(value)
        if (epoch === generation.current) baseline.current = json
        if (isCurrent()) {
          setStatus({ status: 'saved' })
          savedTimer.current = setTimeout(() => {
            if (isCurrent()) setStatus({ status: 'idle' })
          }, visibleMs.current)
        }
        return true
      } catch (e) {
        const err = e as Partial<CommandError>
        if (isCurrent()) setStatus({ status: 'error', kind: err.kind ?? 'internal', message: err.message ?? String(e) })
        return false
      }
    })
    queue.current = operation
    return operation
  }, [])

  useEffect(() => {
    if (timer.current != null) clearTimeout(timer.current)
    if (savedTimer.current != null) clearTimeout(savedTimer.current)
    if (draft == null) {
      generation.current++
      baseline.current = null
      setStatus({ status: 'idle' })
      return
    }
    const json = JSON.stringify(draft)
    if (baseline.current == null) {
      baseline.current = json
      return
    }
    setStatus({ status: 'idle' })
    if (!hasErrors(validateConfig(draft))) {
      timer.current = setTimeout(() => { void flush(draft) }, options.immediate ? 0 : options.textDelayMs)
    }
    return () => {
      if (timer.current != null) clearTimeout(timer.current)
    }
  }, [draft, options.immediate, options.textDelayMs, flush])

  useEffect(() => () => {
    generation.current++
    if (timer.current != null) clearTimeout(timer.current)
    if (savedTimer.current != null) clearTimeout(savedTimer.current)
  }, [])

  return { ...status, flush }
}
