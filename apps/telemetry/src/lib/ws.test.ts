// ws.test.ts — the subscription client's own contract, driven against
// a fake WebSocket: the bounded reconnect state machine, the give-up
// boundary, the manual `retry()` affordance (C28-F6), and the
// typed-envelope decode at the JSON.parse boundary (C28-F7). No real
// sockets — the fake records instances and lets the test fire
// open/message/close synchronously.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import {
  subscribeTelemetry,
  type ConnectionState,
  type TelemetryFrame,
} from './ws'

class FakeWebSocket {
  static instances: FakeWebSocket[] = []
  onopen: ((ev: Event) => void) | null = null
  onmessage: ((ev: MessageEvent) => void) | null = null
  onerror: ((ev: Event) => void) | null = null
  onclose: ((ev: CloseEvent) => void) | null = null
  readonly sent: string[] = []
  closed = false
  constructor(readonly url: string) {
    FakeWebSocket.instances.push(this)
  }
  send(data: string) {
    this.sent.push(data)
  }
  close() {
    if (this.closed) return
    this.closed = true
    this.onclose?.({} as CloseEvent)
  }
  // --- test drivers ---
  open() {
    this.onopen?.({} as Event)
  }
  message(data: unknown) {
    this.onmessage?.({ data: JSON.stringify(data) } as MessageEvent)
  }
}

type StateEvent = [ConnectionState, string | undefined]

function recorder() {
  const states: StateEvent[] = []
  const frames: (TelemetryFrame | null)[] = []
  return {
    states,
    frames,
    onState: (s: ConnectionState, d?: string) => states.push([s, d]),
    onFrame: (f: TelemetryFrame | null) => frames.push(f),
  }
}

beforeEach(() => {
  FakeWebSocket.instances = []
  vi.useFakeTimers()
  vi.stubGlobal('window', {
    location: { href: 'http://localhost/' },
    sessionStorage: {
      getItem: () => null,
      setItem: () => {},
      removeItem: () => {},
    },
    history: { replaceState: () => {} },
  })
  vi.stubGlobal('document', { querySelector: () => null })
  vi.stubGlobal('WebSocket', FakeWebSocket)
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

describe('reconnect state machine', () => {
  it('backs off, gives up, then retry() re-opens with a reset budget', () => {
    const { states, frames, onState, onFrame } = recorder()
    const handle = subscribeTelemetry({
      fields: ['focus.altitude_m'],
      reconnect: { maxAttempts: 2, baseDelayMs: 1000, maxDelayMs: 4000 },
      onFrame,
      onState,
    })
    expect(FakeWebSocket.instances).toHaveLength(1)
    expect(states).toEqual([['connecting', undefined]])

    const ws1 = FakeWebSocket.instances[0]
    ws1.open()
    expect(ws1.sent).toHaveLength(1)
    const sub = JSON.parse(ws1.sent[0]) as { kind: string; fields: string[] }
    expect(sub.kind).toBe('subscribe')
    expect(sub.fields).toEqual(['focus.altitude_m'])
    ws1.message({ proto: 1, kind: 'subscribed', rate_hz: 16 })
    expect(states.at(-1)).toEqual(['live', undefined])

    // Drop: the frame goes stale immediately and the countdown starts.
    ws1.close()
    expect(frames.at(-1)).toBeNull()
    expect(states.at(-1)?.[0]).toBe('closed')
    expect(states.at(-1)?.[1]).toContain('retry 1/2')

    vi.advanceTimersByTime(1000)
    expect(FakeWebSocket.instances).toHaveLength(2)
    expect(states.at(-1)).toEqual(['reconnecting', undefined])
    const ws2 = FakeWebSocket.instances[1]
    ws2.open()
    // Dies mid-handshake — no `subscribed` ack, so the attempt budget
    // must NOT reset (an open that never acks buys back nothing).
    ws2.close()
    expect(states.at(-1)?.[1]).toContain('retry 2/2')

    vi.advanceTimersByTime(2000)
    expect(FakeWebSocket.instances).toHaveLength(3)
    // Third socket dies too — the budget is spent.
    FakeWebSocket.instances[2].close()
    expect(states.at(-1)).toEqual(['closed', 'gave up reconnecting'])

    // Nothing further is scheduled — the old give-up was terminal.
    vi.advanceTimersByTime(60_000)
    expect(FakeWebSocket.instances).toHaveLength(3)

    // C28-F6 — the manual affordance: retry() resets the budget and
    // connects now; a live ack lands 'live' again.
    handle.retry()
    expect(FakeWebSocket.instances).toHaveLength(4)
    expect(states.at(-1)).toEqual(['connecting', undefined])
    const ws4 = FakeWebSocket.instances[3]
    ws4.open()
    ws4.message({ proto: 1, kind: 'subscribed', rate_hz: 16 })
    expect(states.at(-1)).toEqual(['live', undefined])

    // …and a later drop retries on the FULL budget again (2 more
    // sockets, not zero) — the reset was real.
    ws4.close()
    vi.advanceTimersByTime(1000)
    expect(FakeWebSocket.instances).toHaveLength(5)
    handle.close()
  })

  it('retry() is a no-op after close()', () => {
    const { states, onState, onFrame } = recorder()
    const handle = subscribeTelemetry({
      fields: ['tick'],
      onFrame,
      onState,
    })
    handle.close()
    handle.retry()
    expect(FakeWebSocket.instances).toHaveLength(1)
    vi.advanceTimersByTime(60_000)
    expect(FakeWebSocket.instances).toHaveLength(1)
  })
})

describe('wire envelope decode', () => {
  it('decodes telemetry frames through narrowing guards', () => {
    const { frames, onFrame } = recorder()
    subscribeTelemetry({ fields: ['focus.altitude_m'], onFrame })
    const ws = FakeWebSocket.instances[0]
    ws.open()
    ws.message({
      proto: 1,
      kind: 'telemetry',
      seq: 7,
      tick: 42,
      payload: { 'focus.altitude_m': 100.5 },
      unknown_fields: ['bogus.field'],
    })
    expect(frames.at(-1)).toEqual({
      seq: 7,
      tick: 42,
      payload: { 'focus.altitude_m': 100.5 },
      unknown_fields: ['bogus.field'],
    })
    // A sparse/wrongly-shaped frame degrades field-by-field — it
    // must never throw into onmessage.
    ws.message({ proto: 1, kind: 'telemetry' })
    expect(frames.at(-1)).toEqual({
      seq: 0,
      tick: 0,
      payload: {},
      unknown_fields: [],
    })
    ws.message({ proto: 1, kind: 'telemetry', unknown_fields: 'oops' })
    expect(frames.at(-1)?.unknown_fields).toEqual([])
  })

  it('gates on proto and surfaces wire errors', () => {
    const { states, frames, onState, onFrame } = recorder()
    subscribeTelemetry({ fields: ['tick'], onFrame, onState })
    const ws = FakeWebSocket.instances[0]
    ws.open()
    // Wrong proto → error state + stale frame.
    ws.message({ proto: 2, kind: 'telemetry' })
    expect(frames.at(-1)).toBeNull()
    expect(states.at(-1)?.[0]).toBe('error')
    expect(states.at(-1)?.[1]).toContain('wire proto')
    // Server-side `error` kind likewise.
    ws.message({ proto: 1, kind: 'error', error: 'bad fields' })
    expect(frames.at(-1)).toBeNull()
    expect(states.at(-1)).toEqual(['error', 'bad fields'])
    // Non-object / non-JSON messages are ignored entirely.
    ws.message('not json')
    ws.message(42)
    ws.message([1, 2])
    expect(states.at(-1)).toEqual(['error', 'bad fields'])
  })
})
