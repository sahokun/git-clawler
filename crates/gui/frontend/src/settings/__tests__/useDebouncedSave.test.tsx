import { act, useState } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { saveConfig } from '@/api'
import type { AppConfigDto } from '@/types'
import { useDebouncedSave } from '../useDebouncedSave'

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

vi.mock('@/api', () => ({
  saveConfig: vi.fn(() => Promise.resolve()),
}))

function config(refreshIntervalSecs = 30): AppConfigDto {
  return {
    general: { refresh_interval_secs: refreshIntervalSecs, auto_fetch: false },
    repositories: [],
    scan_roots: [],
    github: { token_env: null, enabled: false, enable_issues: true, enable_prs: true },
    notifications: { enabled: false, cooldown_secs: 300 },
    gui: {
      theme: 'dark',
      start_in_tray: false,
      hide_from_taskbar: false,
      close_to_tray: true,
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

describe('useDebouncedSave', () => {
  let root: Root
  let container: HTMLDivElement
  let flush: ReturnType<typeof useDebouncedSave>['flush']
  let setDraft: (draft: AppConfigDto) => void

  beforeEach(() => {
    vi.useFakeTimers()
    vi.mocked(saveConfig).mockClear()
    container = document.createElement('div')
    document.body.appendChild(container)
    root = createRoot(container)
  })

  afterEach(() => {
    act(() => root.unmount())
    container.remove()
    vi.useRealTimers()
  })

  function Harness() {
    const [draft, updateDraft] = useState(config())
    setDraft = updateDraft
    const status = useDebouncedSave(draft, {
      textDelayMs: 100,
      immediate: false,
      savedVisibleMs: 3000,
    })
    flush = status.flush
    return <div>{status.status}</div>
  }

  async function advance(ms: number) {
    await act(async () => {
      vi.advanceTimersByTime(ms)
      await Promise.resolve()
    })
  }

  it('hides Saved after the visible timeout and shows it again on later saves', async () => {
    act(() => root.render(<Harness />))

    act(() => setDraft(config(31)))
    await advance(100)

    expect(saveConfig).toHaveBeenCalledTimes(1)
    expect(container.textContent).toBe('saved')

    await advance(2999)
    expect(container.textContent).toBe('saved')

    await advance(1)
    expect(container.textContent).toBe('idle')

    act(() => setDraft(config(32)))
    await advance(100)

    expect(saveConfig).toHaveBeenCalledTimes(2)
    expect(container.textContent).toBe('saved')
  })
  it('serializes an in-flight autosave and the close flush', async () => {
    let finish: () => void = () => {}
    vi.mocked(saveConfig).mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve }))
    act(() => root.render(<Harness />))
    act(() => setDraft(config(31)))
    await advance(100)
    act(() => setDraft(config(32)))
    let closed: Promise<boolean>
    await act(async () => { closed = flush(config(32)) })
    expect(saveConfig).toHaveBeenCalledTimes(1)
    await act(async () => { finish(); await closed })
    expect(saveConfig).toHaveBeenCalledTimes(2)
    expect(vi.mocked(saveConfig).mock.calls[1][0].general.refresh_interval_secs).toBe(32)
    expect(container.textContent).toBe('saved')
  })

  it('restores the baseline when the user reverts an in-flight edit', async () => {
    let finish: () => void = () => {}
    vi.mocked(saveConfig).mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve }))
    act(() => root.render(<Harness />))
    act(() => setDraft(config(31)))
    await advance(100)
    act(() => setDraft(config(30)))
    await advance(100)
    expect(saveConfig).toHaveBeenCalledTimes(1)
    await act(async () => { finish(); await Promise.resolve() })
    expect(saveConfig).toHaveBeenCalledTimes(2)
    expect(vi.mocked(saveConfig).mock.calls[1][0].general.refresh_interval_secs).toBe(30)
  })

  it('refuses to close an invalid draft without writing it', async () => {
    act(() => root.render(<Harness />))
    act(() => setDraft(config(0)))
    await act(async () => { expect(await flush(config(0))).toBe(false) })
    expect(saveConfig).not.toHaveBeenCalled()
  })

})