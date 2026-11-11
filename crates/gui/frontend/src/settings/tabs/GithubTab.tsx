import type { AppConfigDto } from '@/types'
import type { ValidationErrors } from '../settingsValidation'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'

interface Props {
  draft: AppConfigDto
  onChange: (next: AppConfigDto, immediate: boolean) => void
  errors: ValidationErrors
}

export function GithubTab({ draft, onChange, errors }: Props) {
  const tokenError = errors['github.token_env']
  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <Checkbox
          id="github-enabled"
          checked={draft.github.enabled}
          onCheckedChange={checked =>
            onChange(
              {
                ...draft,
                github: { ...draft.github, enabled: checked === true },
              },
              true,
            )
          }
        />
        <Label htmlFor="github-enabled">Enable GitHub issue / PR counts</Label>
      </div>
      <div className="flex gap-4 text-sm text-gray-700 dark:text-gray-200">
        <div className="flex items-center gap-2">
          <Checkbox
            id="github-issues"
            checked={draft.github.enable_issues}
            onCheckedChange={checked =>
              onChange(
                {
                  ...draft,
                  github: { ...draft.github, enable_issues: checked === true },
                },
                true,
              )
            }
          />
          <Label htmlFor="github-issues">Issues</Label>
        </div>
        <div className="flex items-center gap-2">
          <Checkbox
            id="github-prs"
            checked={draft.github.enable_prs}
            onCheckedChange={checked =>
              onChange(
                {
                  ...draft,
                  github: { ...draft.github, enable_prs: checked === true },
                },
                true,
              )
            }
          />
          <Label htmlFor="github-prs">PRs</Label>
        </div>
      </div>
      <div className="space-y-2">
        <Label htmlFor="token_env">Token environment variable name</Label>
        <Input
          id="token_env"
          value={draft.github.token_env ?? ''}
          placeholder="GITHUB_TOKEN"
          invalid={!!tokenError}
          onChange={e =>
            onChange(
              {
                ...draft,
                github: {
                  ...draft.github,
                  token_env: e.target.value === '' ? null : e.target.value,
                },
              },
              false,
            )
          }
        />
        {tokenError ? (
          <p className="text-xs text-red-600 dark:text-red-400">{tokenError}</p>
        ) : (
          <p className="text-xs text-gray-500 dark:text-gray-400">
            Leave blank to use <code>GITHUB_TOKEN</code>.
          </p>
        )}
      </div>
    </div>
  )
}
