import { useState } from 'react'
import type { AppConfigDto } from '@/types'
import type { ValidationErrors } from '../settingsValidation'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { captureShortcut } from '../shortcutCapture'

interface Props {
  draft: AppConfigDto
  onChange: (next: AppConfigDto, immediate: boolean) => void
  errors: ValidationErrors
}

export function ShortcutTab({ draft, onChange, errors }: Props) {
  const [capturing, setCapturing] = useState(false)
  const shortcutError = errors['gui.global_shortcut']

  const setGui = (patch: Partial<AppConfigDto['gui']>, immediate: boolean) => {
    onChange({ ...draft, gui: { ...draft.gui, ...patch } }, immediate)
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <Checkbox
          id="start_in_tray"
          checked={draft.gui.start_in_tray}
          onCheckedChange={c => setGui({ start_in_tray: c === true }, true)}
        />
        <Label htmlFor="start_in_tray">Start in tray</Label>
      </div>
      <div className="flex items-center gap-2">
        <Checkbox
          id="hide_from_taskbar"
          checked={draft.gui.hide_from_taskbar}
          onCheckedChange={c => setGui({ hide_from_taskbar: c === true }, true)}
        />
        <Label htmlFor="hide_from_taskbar">Hide from taskbar</Label>
      </div>
      <div className="flex items-center gap-2">
        <Checkbox
          id="close_to_tray"
          checked={draft.gui.close_to_tray}
          onCheckedChange={c => setGui({ close_to_tray: c === true }, true)}
        />
        <Label htmlFor="close_to_tray">Close to tray</Label>
      </div>

      <div className="space-y-2">
        <Label htmlFor="global_shortcut">Global shortcut</Label>
        <div className="flex gap-2">
          <Input
            id="global_shortcut"
            readOnly
            value={draft.gui.global_shortcut ?? ''}
            invalid={!!shortcutError}
            placeholder={capturing ? 'Press keys...' : 'Not set'}
            onFocus={() => setCapturing(true)}
            onBlur={() => setCapturing(false)}
            onKeyDown={e => {
              e.preventDefault()
              e.stopPropagation()
              const captured = captureShortcut(e.nativeEvent)
              if (captured === undefined) return
              if (captured === null) {
                ;(e.target as HTMLInputElement).blur()
                return
              }
              setGui({ global_shortcut: captured }, true)
            }}
          />
          <Button
            variant="outline"
            size="sm"
            onClick={() => setGui({ global_shortcut: null }, true)}
          >
            Clear
          </Button>
        </div>
        {shortcutError && <p className="text-xs text-red-600 dark:text-red-400">{shortcutError}</p>}
        <p className="text-xs text-gray-500 dark:text-gray-400">
          Some OS-reserved shortcuts cannot be captured. Press Esc to cancel.
        </p>
      </div>
    </div>
  )
}
