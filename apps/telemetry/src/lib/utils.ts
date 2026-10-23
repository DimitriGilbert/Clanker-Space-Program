import { clsx, type ClassValue } from 'clsx'
import { twMerge } from 'tailwind-merge'

/** The shadcn `cn` helper — conditional class merge. */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}
