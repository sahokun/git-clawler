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

export function GeneralTab({ draft, onChange, errors }: Props) {
  return (
    <div className="space-y-4">
      <div className="space-y-2">
        <Label htmlFor="refresh_interval">Refresh interval (seconds)</Label>
        <Input
          id="refresh_interval"
          type="number"
          min={1}
          max={86400}
          value={draft.general.refresh_interval_secs}
          invalid={!!errors['general.refresh_interval_secs']}
          onChange={e => {
            const value = Number(e.target.value)
            onChange(
              {
                ...draft,
                general: { ...draft.general, refresh_interval_secs: value },
              },
              false,
            )
          }}
        />
        {errors['general.refresh_interval_secs'] && (
          <p className="text-xs text-red-600 dark:text-red-400">{errors['general.refresh_interval_secs']}</p>
        )}
      </div>

      <div className="flex items-center gap-2">
        <Checkbox
          id="auto_fetch"
          checked={draft.general.auto_fetch}
          onCheckedChange={checked => {
            onChange(
              {
                ...draft,
                general: { ...draft.general, auto_fetch: checked === true },
              },
              true,
            )
          }}
        />
        <Label htmlFor="auto_fetch">Auto-fetch on refresh</Label>
      </div>
    </div>
  )
}
