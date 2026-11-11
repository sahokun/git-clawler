import { describe, expect, it } from 'vitest'
import type { RepoRow } from '../types'
import { createElement } from 'react'
import { renderToStaticMarkup } from 'react-dom/server'
import { DetailPanel, RepoListRow, hasChanges, githubUrlFromRemote, isWslRepoPath, repoKey, toViewModel } from '../hybridUi'

const detailProps = {
  onClose: () => {},
  onOpenFolder: () => {},
  onOpenGithub: () => {},
}

function row(path: string, distro: string | null): RepoRow {
  return {
    index: 0,
    name: '',
    path,
    is_current: false,
    scanned_at: '2026-05-06T12:00:00Z',
    distro,
    remote_url: null,
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

it('shows failed-fetch freshness alongside cached status and keeps it in changes-only views', () => {
  const example = row('C:/projects/example', null)
  if (example.status.kind !== 'ok') throw new Error('invalid fixture')
  example.status.value.fetch_warning = 'fetch connection failed; using cached remote refs'
  const view = toViewModel(example)
  const markup = renderToStaticMarkup(createElement(RepoListRow, {
    view, selected: false, onSelect: () => {}, gridTemplateColumns: '400px',
  }))
  expect(markup).toContain('fetch connection failed; using cached remote refs')
  expect(view.uncommitted).toBe(0)
  expect(view.push).toBe(0)
  expect(hasChanges(view)).toBe(true)
  example.status.value.fetch_warning = null
  expect(hasChanges(toViewModel(example))).toBe(false)
})

describe('hybrid UI host classification', () => {
  it('treats Linux absolute paths as WSL even when distro is omitted', () => {
    const view = toViewModel(row('/home/user/project', null))

    expect(isWslRepoPath('/home/user/project')).toBe(true)
    expect(view.host).toBe('wsl')
    expect(view.hostLabel).toBe('WSL · default')
  })

  it('treats Windows paths as native', () => {
    const view = toViewModel(row('D:\\repos\\project', null))

    expect(isWslRepoPath('D:\\repos\\project')).toBe(false)
    expect(view.host).toBe('native')
    expect(view.hostLabel).toBe('Native')
  })

  it('includes host and distro in stable row keys', () => {
    expect(repoKey(row('/home/user/project', null))).not.toEqual(repoKey(row('/home/user/project', 'Ubuntu')))
  })
})

describe('GitHub remote URL normalization', () => {
  it('normalizes supported GitHub remote URL formats', () => {
    expect(githubUrlFromRemote('https://github.com/sahokun/git-clawler.git')).toBe('https://github.com/sahokun/git-clawler')
    expect(githubUrlFromRemote('git@github.com:sahokun/git-clawler.git')).toBe('https://github.com/sahokun/git-clawler')
    expect(githubUrlFromRemote('ssh://git@github.com/sahokun/git-clawler.git')).toBe('https://github.com/sahokun/git-clawler')
  })

  it('rejects non-GitHub or unsafe remotes', () => {
    expect(githubUrlFromRemote('https://gitlab.com/sahokun/git-clawler.git')).toBeNull()
    expect(githubUrlFromRemote('https://github.com/sahokun/git-clawler/tree/main')).toBeNull()
    expect(githubUrlFromRemote('https://github.com/-bad/git-clawler.git')).toBeNull()
  })
})

describe('DetailPanel recent commits', () => {
  it('renders real recent commit data', () => {
    const data = row('/home/user/project', 'Ubuntu')
    if (data.status.kind === 'ok') {
      data.status.value.recent_commits = [{
        short_hash: 'abc1234',
        summary: 'Add recent commits UI',
        author_name: 'Alice',
        committed_at: '2026-05-06T12:34:56Z',
      }]
    }

    const html = renderToStaticMarkup(createElement(DetailPanel, { view: toViewModel(data), ...detailProps }))

    expect(html).toContain('abc1234')
    expect(html).toContain('Add recent commits UI')
    expect(html).toContain('Alice')
    expect(html).not.toContain('Backend not connected')
  })

  it('renders empty state when recent commits are absent', () => {
    const html = renderToStaticMarkup(createElement(DetailPanel, { view: toViewModel(row('/home/user/project', null)), ...detailProps }))

    expect(html).toContain('No recent commits found.')
  })
})
