// shadcn-style Card — the vendored-component convention: components/ui
// are generated into the repo and customized in place.
import * as React from 'react'
import { cn } from '~/lib/utils'

export function Card({ className, ...props }: React.ComponentProps<'div'>) {
  return (
    <div
      data-slot="card"
      className={cn('widget-tile', className)}
      {...props}
    />
  )
}

export function CardHeader({ className, ...props }: React.ComponentProps<'div'>) {
  return <div className={cn('widget-title', className)} {...props} />
}

export function CardContent({ className, ...props }: React.ComponentProps<'div'>) {
  return <div className={cn('flex-1 min-h-0', className)} {...props} />
}
