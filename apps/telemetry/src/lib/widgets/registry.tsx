// M9d — the widget registry: kind → renderer. The server's
// dashboard.rs mirrors the kind set (its own `WIDGET_KINDS` —
// this module's map keys); adding a kind is additive in both places.
import type { ComponentType } from 'react'
import type { TelemetryFrame } from '../ws'
import type { WidgetDef, WidgetKind } from '../dashboard'
import { GaugeWidget } from './gauge'
import { GraphWidget } from './graph'
import { TableWidget } from './table'
import { OrbitMapWidget } from './orbit-map'

/** What every widget receives — its def + the latest frame's
 *  resolved fields (path → value; missing paths are absent). `frame`
 *  is `null` before the first frame and whenever the stream has gone
 *  stale (socket closed/errored). */
export interface WidgetProps {
  def: WidgetDef
  frame: TelemetryFrame | null
  /** The rate the server negotiated in its `subscribed` ack — widgets
   *  size time-based buffers from this, never a hardcoded 16 Hz. */
  rateHz: number
}

export const WIDGET_REGISTRY: Record<WidgetKind, ComponentType<WidgetProps>> = {
  gauge: GaugeWidget,
  graph: GraphWidget,
  table: TableWidget,
  'orbit-map': OrbitMapWidget,
}
