/**
 * Native bridge for the EchoSpace editor.
 *
 * Same wire contract every built-in editor speaks: the host posts
 * `futureboard.selectInstance` with the authoritative state, the page answers
 * `futureboard.instanceReady`, and every gesture travels back as a batched
 * `futureboard.setParams` tagged with the binding it was made against. The
 * host drops a batch whose `bindingGeneration` is stale, so an edit made
 * against a torn-down instance can never land on its replacement.
 */

import {
  DEFAULT_DIVISION_L,
  DEFAULT_DIVISION_R,
  DIVISION_BEATS,
  MODES,
  type Mode,
} from './params'

export const BRIDGE_PROTOCOL_VERSION = 1
export const PLUGIN_ID = 'echospace'

/** The state blob `echospace::ipc::EchospaceState` serializes. */
export type EchoParams = {
  power: boolean
  mode: Mode
  timeMsL: number
  timeMsR: number
  feedback: number
  crossFeedback: number
  lowCutHz: number
  highCutHz: number
  saturation: number
  mix: number
  outputDb: number
  freeze: boolean
  /** Delay times follow the host tempo and the divisions below. */
  sync: boolean
  /** Index into `DIVISION_LABELS`, used while `sync` is on. */
  divisionL: number
  divisionR: number
  /** Both sides move together. */
  link: boolean
}

type Binding = {
  pluginId: string
  instanceId: string
  bindingGeneration: number
}

type SelectInstanceMessage = {
  type: 'futureboard.selectInstance'
  protocolVersion: number
  pluginId: string
  instanceId: string
  bindingGeneration: number
  stateRevision: number
  state: unknown
}

type InstanceRemovedMessage = {
  type: 'futureboard.instanceRemoved'
  protocolVersion: number
  instanceId: string
}

/**
 * Low-rate (~1 Hz) host telemetry. Only `tempoBpm` is consumed here: a synced
 * delay time is a note length, and the editor cannot turn that into the
 * milliseconds it prints — or into the echo picture — without the tempo the
 * DSP is actually running against.
 */
type HostStatusMessage = {
  type: 'futureboard.hostStatus'
  protocolVersion: number
  instanceId: string
  sampleRate: number
  blockSize: number
  latencySamples: number
  tempoBpm: number
}

let binding: Binding | null = null
const pending = new Map<string, number>()
let scheduled = false

function post(body: unknown) {
  if (window.location.protocol !== 'mikoplugin:') return
  try {
    void fetch('__bridge', {
      method: 'POST',
      body: JSON.stringify(body),
    }).catch(() => {})
  } catch {
    // A standalone design preview intentionally has no native endpoint.
  }
}

/**
 * Coalesce a frame's worth of edits into one batch. A knob drag emits on every
 * pointer move; the map keeps the *last* value per id, so the committed value
 * is always sent even though the intermediate ones are not.
 */
function flush() {
  scheduled = false
  if (!binding || pending.size === 0) {
    pending.clear()
    return
  }
  const params = Array.from(pending, ([id, value]) => ({ id, value }))
  pending.clear()
  post({
    type: 'futureboard.setParams',
    protocolVersion: BRIDGE_PROTOCOL_VERSION,
    ...binding,
    params,
  })
}

export function postParam(id: string, value: number) {
  pending.set(id, value)
  if (scheduled) return
  scheduled = true
  requestAnimationFrame(flush)
}

const NUMERIC_KEYS = [
  'timeMsL',
  'timeMsR',
  'feedback',
  'crossFeedback',
  'lowCutHz',
  'highCutHz',
  'saturation',
  'mix',
  'outputDb',
] as const

/** Clamp a division index out of a blob onto the table Rust indexes with. */
function parseDivision(value: unknown, fallback: number): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) return fallback
  return Math.min(Math.max(Math.round(value), 0), DIVISION_BEATS.length - 1)
}

/**
 * Accept a host state blob only when every field is present and of the right
 * type. A partial blob is rejected whole rather than merged: a half-applied
 * state would show values the DSP does not have.
 *
 * The tempo-sync fields are the exception, and deliberately so: projects saved
 * before EchoSpace had them carry no `sync`/`division*`/`link`, and Rust
 * restores those blobs with the same defaults rather than rejecting them.
 * Rejecting here would blank an editor over state the DSP accepted.
 */
function parseParams(state: unknown): EchoParams | null {
  if (!state || typeof state !== 'object') return null
  const candidate =
    'params' in state ? (state as { params?: unknown }).params : state
  if (!candidate || typeof candidate !== 'object') return null
  const params = candidate as Record<string, unknown>

  if (typeof params.power !== 'boolean' || typeof params.freeze !== 'boolean') {
    return null
  }
  if (!MODES.includes(params.mode as Mode)) return null
  for (const key of NUMERIC_KEYS) {
    const value = params[key]
    if (typeof value !== 'number' || !Number.isFinite(value)) return null
  }
  return {
    ...(params as unknown as EchoParams),
    sync: params.sync === true,
    link: params.link === true,
    divisionL: parseDivision(params.divisionL, DEFAULT_DIVISION_L),
    divisionR: parseDivision(params.divisionR, DEFAULT_DIVISION_R),
  }
}

export function connectBridge(
  onParams: (params: EchoParams) => void,
  onConnection: (connected: boolean) => void,
  onTempo?: (tempoBpm: number) => void,
) {
  post({
    type: 'futureboard.bridgeReady',
    protocolVersion: BRIDGE_PROTOCOL_VERSION,
    bridgeVersion: BRIDGE_PROTOCOL_VERSION,
    pluginId: PLUGIN_ID,
  })

  const listener = (event: MessageEvent) => {
    const message = event.data as
      | SelectInstanceMessage
      | InstanceRemovedMessage
      | HostStatusMessage
      | undefined
    if (!message || typeof message !== 'object') return

    if (message.type === 'futureboard.hostStatus') {
      if (binding?.instanceId !== message.instanceId) return
      if (typeof message.tempoBpm === 'number' && message.tempoBpm > 0) {
        onTempo?.(message.tempoBpm)
      }
      return
    }

    if (message.type === 'futureboard.selectInstance') {
      binding = {
        pluginId: message.pluginId,
        instanceId: message.instanceId,
        bindingGeneration: message.bindingGeneration,
      }
      // Edits queued against the previous binding are abandoned, not
      // re-tagged: they were made against different state.
      pending.clear()
      const params = parseParams(message.state)
      if (params) onParams(params)
      onConnection(true)
      post({
        type: 'futureboard.instanceReady',
        protocolVersion: BRIDGE_PROTOCOL_VERSION,
        pluginId: message.pluginId,
        instanceId: message.instanceId,
        bindingGeneration: message.bindingGeneration,
        stateRevision: message.stateRevision,
      })
    } else if (
      message.type === 'futureboard.instanceRemoved' &&
      binding?.instanceId === message.instanceId
    ) {
      binding = null
      pending.clear()
      onConnection(false)
    }
  }

  window.addEventListener('message', listener)
  return () => {
    window.removeEventListener('message', listener)
    binding = null
    pending.clear()
  }
}
