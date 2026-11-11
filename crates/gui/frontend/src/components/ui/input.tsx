import * as React from 'react'
import { cn } from '@/lib/utils'

export interface InputProps extends React.InputHTMLAttributes<HTMLInputElement> {
  invalid?: boolean
}

export const Input = React.forwardRef<HTMLInputElement, InputProps>(
  ({ className, invalid, type = 'text', ...props }, ref) => (
    <input
      ref={ref}
      type={type}
      className={cn(
        'flex h-9 w-full rounded-md border bg-white px-3 py-1 text-sm text-gray-900 shadow-sm transition-colors',
        'placeholder:text-gray-400 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 dark:bg-gray-950 dark:text-gray-100 dark:placeholder:text-gray-500',
        'disabled:cursor-not-allowed disabled:opacity-50',
        invalid ? 'border-red-500 focus-visible:ring-red-500' : 'border-gray-300 dark:border-gray-700',
        className,
      )}
      {...props}
    />
  ),
)
Input.displayName = 'Input'
