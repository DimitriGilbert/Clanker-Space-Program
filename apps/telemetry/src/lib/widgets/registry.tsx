// M9d — the widget registry: kind → renderer. The server's
// dashboard.rs mirrors the kind list (`WIDGET_KINDS`); adding a kind
// is additive in both places.
import type { ComponentType } from 'react'
import type { TelemetryFrame } from '../ws'
import type { WidgetDef, WidgetKind } from '../dashboard'
import { GaugeWidget } from './gauge'
import { GraphWidget } from './graph'
import { TableWidget } from './table'
import { OrbitMapWidget } from './orbit-map'

/** What every widget receives — its def + the latest frame's
 *  resolved fields (path → value; missing paths are absent). */
export interface WidgetProps {
  def: WidgetDef
  frame: TelemetryFrame | null
}

export const WIDGET_REGISTRY: Record<WidgetKind, ComponentType<WidgetProps>> = {
  gauge: GaugeWidget,
  graph: GraphWidget,
  table: TableWidget,
  'orbit-map': OrbitMapWidget,
}

export const WIDGET_KINDS = Object.keys(WIDGET_REGISTRY) as WidgetKind[]
