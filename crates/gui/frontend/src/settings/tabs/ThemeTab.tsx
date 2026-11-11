import type { AppConfigDto } from '@/types'
import type { ValidationErrors } from '../settingsValidation'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { Moon, Sun } from 'lucide-react'

interface Props {
  draft: AppConfigDto
  onChange: (next: AppConfigDto, immediate: boolean) => void
  errors: ValidationErrors
}

export function ThemeTab({ draft, onChange, errors }: Props) {
  const setTheme = (theme: AppConfigDto['gui']['theme']) => {
    onChange({ ...draft, gui: { ...draft.gui, theme } }, true)
  }

  return (
    <div className="space-y-4">
      <div className="space-y-2">
        <Label>Theme</Label>
        <div
          className="inline-flex rounded-md border border-gray-300 bg-white p-1 dark:border-gray-700 dark:bg-gray-900"
          role="group"
          aria-label="Theme"
        >
          <Button
            variant={draft.gui.theme === 'dark' ? 'default' : 'ghost'}
            size="sm"
            onClick={() => setTheme('dark')}
            aria-pressed={draft.gui.theme === 'dark'}
          >
            <Moon className="h-4 w-4" />
            Dark
          </Button>
          <Button
            variant={draft.gui.theme === 'light' ? 'default' : 'ghost'}
            size="sm"
            onClick={() => setTheme('light')}
            aria-pressed={draft.gui.theme === 'light'}
          >
            <Sun className="h-4 w-4" />
            Light
          </Button>
        </div>
        {errors['gui.theme'] && <p className="text-xs text-red-600 dark:text-red-400">{errors['gui.theme']}</p>}
      </div>
    </div>
  )
}