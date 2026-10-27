import { clsx, type ClassValue } from 'clsx'
import { twMerge } from 'tailwind-merge'

/** The shadcn `cn` helper — conditional class merge. */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

/** The finite-number guard every widget shares — telemetry values
 *  arrive as `unknown` off the wire, so a present-but-non-numeric or
 *  non-finite payload resolves to `null` (the "no data" branch, not
 *  a rendered `NaN`). */
export function num(v: unknown): number | null {
  return typeof v === 'number' && Number.isFinite(v) ? v : null
}

/** The plain-object guard — the `JSON.parse` / payload-dispatch
 *  boundary's narrowing step. Arrays and `null` are not records. */
export function asRecord(v: unknown): Record<string, unknown> | null {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
    ? (v as Record<string, unknown>)
    : null
}
