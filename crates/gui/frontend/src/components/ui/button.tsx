import * as React from 'react'
import { cn } from '@/lib/utils'

type Variant = 'default' | 'outline' | 'ghost' | 'destructive'
type Size = 'sm' | 'default' | 'icon'

const variants: Record<Variant, string> = {
  default: 'bg-blue-600 text-white hover:bg-blue-700',
  outline: 'border border-gray-300 bg-white hover:bg-gray-50 text-gray-900 dark:border-gray-700 dark:bg-gray-900 dark:hover:bg-gray-800 dark:text-gray-100',
  ghost: 'hover:bg-gray-100 text-gray-900 dark:hover:bg-gray-800 dark:text-gray-100',
  destructive: 'bg-red-600 text-white hover:bg-red-700',
}

const sizes: Record<Size, string> = {
  sm: 'h-8 px-3 text-xs',
  default: 'h-9 px-4 text-sm',
  icon: 'h-9 w-9 p-0',
}

export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant
  size?: Size
}

export const Button = React.forwardRef<HTMLButtonElement, ButtonProps>(
  ({ className, variant = 'default', size = 'default', type = 'button', ...props }, ref) => (
    <button
      ref={ref}
      type={type}
      className={cn(
        'inline-flex items-center justify-center gap-2 rounded-md font-medium transition-colors disabled:opacity-50 disabled:pointer-events-none focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-blue-500',
        variants[variant],
        sizes[size],
        className,
      )}
      {...props}
    />
  ),
)
Button.displayName = 'Button'
