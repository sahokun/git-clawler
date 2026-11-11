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

export function NotificationsTab({ draft, onChange, errors }: Props) {
  const cooldownError = errors['notifications.cooldown_secs']

  const setNotifications = (
    patch: Partial<AppConfigDto['notifications']>,
    immediate: boolean,
  ) => {
    onChange(
      {
        ...draft,
        notifications: { ...draft.notifications, ...patch },
      },
      immediate,
    )
  }

  return (
    <div className="space-y-4">
      <p className="text-sm text-gray-500 dark:text-gray-400">Windows notifications require the MSI installation. Standalone portable builds do not support notifications.</p>
      <div className="flex items-center gap-2">
        <Checkbox
          id="notifications-enabled"
          checked={draft.notifications.enabled}
          onCheckedChange={checked => setNotifications({ enabled: checked === true }, true)}
        />
        <Label htmlFor="notifications-enabled">Enable notifications</Label>
      </div>

      <div className="space-y-2">
        <Label htmlFor="notifications-cooldown">Cooldown (seconds)</Label>
        <Input
          id="notifications-cooldown"
          type="number"
          min={1}
          max={86400}
          value={draft.notifications.cooldown_secs}
          invalid={!!cooldownError}
          onChange={e => setNotifications({ cooldown_secs: Number(e.target.value) }, false)}
        />
        {cooldownError ? (
          <p className="text-xs text-red-600 dark:text-red-400">{cooldownError}</p>
        ) : (
          <p className="text-xs text-gray-500 dark:text-gray-400">
            Minimum wait before sending a similar notification again. Use 1 to 86400 seconds.
          </p>
        )}
      </div>
    </div>
  )
}
