import { describe, expect, it } from 'vitest'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import type { AppConfigDto, RepoEntryDto, RepoRow } from '../types'
import { DetailPanel, RepoListRow, groupWorktrees, summarize, toViewModel } from '../hybridUi'
import { mergeExternalRepositories } from '../settings/externalMerge'

function row(path: string, worktreeOf: string | null = null, distro: string | null = null): RepoRow {
  return {
    index: 0,
    name: '',
    path,
    is_current: false,
    scanned_at: '2026-11-16T00:00:00Z',
    distro,
    remote_url: null,
    worktree_of: worktreeOf,
    status: {
      kind: 'ok',
      value: {
        current_branch: 'main',
        uncommitted_files: 0,
        unpushed_branches: [],
        ahead: 0,
        behind: 0,
        open_issues: null,
        open_prs: null,
        conflict_predicted: null,
        recent_commits: [],
      },
    },
  }
}

describe('worktree grouping', () => {
  it('places worktrees directly after their main repository', () => {
    const views = [
      row('C:/p/app-wt', 'C:\\p\\app'),
      row('C:/p/lib'),
      row('C:/p/app'),
      row('C:/elsewhere/orphan-wt', 'C:/gone/main'),
    ].map(toViewModel)
    const ordered = groupWorktrees(views)
    expect(ordered.map(v => [v.row.path, v.nested])).toEqual([
      ['C:/p/lib', false],
      ['C:/p/app', false],
      ['C:/p/app-wt', true],
      ['C:/elsewhere/orphan-wt', false],
    ])
  })

  it('matches WSL parents case-sensitively and per distro', () => {
    const ordered = groupWorktrees([
      row('/home/u/app'),
      row('/home/u/app-wt', '/home/u/App'),
      row('/home/u/app-wt2', '/home/u/app', 'Other'),
    ].map(toViewModel))
    expect(ordered.every(v => !v.nested)).toBe(true)
  })

  it('marks worktree rows and counts them separately', () => {
    const views = groupWorktrees([row('C:/p/app'), row('C:/p/app-wt', 'C:/p/app')].map(toViewModel))
    const html = renderToStaticMarkup(createElement(RepoListRow, {
      view: views[1], selected: false, onSelect: () => {}, gridTemplateColumns: '1fr',
    }))
    expect(html).toContain('is-worktree')
    expect(html).toContain('worktree')
    expect(summarize(views)).toMatchObject({ repos: 1, worktrees: 1 })
  })
})

describe('not found rows', () => {
  it('shows NOT FOUND and a prominent Remove action', () => {
    const missing: RepoRow = { ...row('C:/p/gone'), status: { kind: 'not_found', value: 'path not found: C:/p/gone' } }
    const view = toViewModel(missing)
    expect(view.notFound).toBe(true)
    expect(view.branch).toBe('NOT FOUND')
    const html = renderToStaticMarkup(createElement(DetailPanel, {
      view, onClose: () => {}, onOpenFolder: () => {}, onOpenGithub: () => {}, onRemove: () => {},
    }))
    expect(html).toContain('Path not found')
    expect(html).toContain('Remove')
  })
})

function entry(path: string, name: string | null = null): RepoEntryDto {
  return { name, path, fetch: null, distro: null }
}

function config(repositories: RepoEntryDto[], exclude: string[] = []): AppConfigDto {
  return {
    general: { refresh_interval_secs: 30, auto_fetch: false },
    repositories,
    scan_roots: [{ path: 'C:/p', depth: 1, exclude, distro: null, auto: true, worktrees: true }],
    github: { token_env: null, enabled: false, enable_issues: true, enable_prs: true },
    notifications: { enabled: false, cooldown_secs: 300 },
    gui: {} as AppConfigDto['gui'],
  }
}

describe('mergeExternalRepositories', () => {
  it('adds discovered entries, drops removed ones and keeps user edits', () => {
    const previous = config([entry('C:/p/a'), entry('C:/p/old')])
    const draft = config([entry('C:/p/a', 'renamed'), entry('C:/p/old'), entry('')])
    const disk = config([entry('C:/p/a'), entry('C:/p/new')], ['old'])
    const merged = mergeExternalRepositories(draft, previous, disk)
    expect(merged.repositories).toEqual([entry('C:/p/a', 'renamed'), entry('C:/p/new'), entry('')])
    expect(merged.scan_roots[0].exclude).toEqual(['old'])
  })
})
