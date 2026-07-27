/**
 * Native bridge for the VerbSpace editor.
 *
 * Same wire contract every built-in editor speaks: the host posts
 * `futureboard.selectInstance` with the authoritative state, the page answers
 * `futureboard.instanceReady`, and every gesture travels back as a batched
 * `futureboard.setParams` tagged with the binding it was made against. The
 * host drops a batch whose `bindingGeneration` is stale, so an edit made
 * against a torn-down instance can never land on its replacement.
 */

import { MODES, type Mode } from './params'

export const BRIDGE_PROTOCOL_VERSION = 1
export const PLUGIN_ID = 'verbspace'

/** The state blob `verbspace::ipc::VerbspaceState` serializes. */
export type VerbParams = {
  power: boolean
  mode: Mode
  predelayMs: number
  size: number
  decaySec: number
  diffusion: number
  damping: number
  bassMult: number
  modDepth: number
  modRateHz: number
  lowCutHz: number
  highCutHz: number
  width: number
  mix: number
  outputDb: number
  freeze: boolean
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
  'predelayMs',
  'size',
  'decaySec',
  'diffusion',
  'damping',
  'bassMult',
  'modDepth',
  'modRateHz',
  'lowCutHz',
  'highCutHz',
  'width',
  'mix',
  'outputDb',
] as const

/**
 * Accept a host state blob only when every field is present and of the right
 * type. A partial blob is rejected whole rather than merged: a half-applied
 * state would show values the DSP does not have.
 */
function parseParams(state: unknown): VerbParams | null {
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
  return params as unknown as VerbParams
}

export function connectBridge(
  onParams: (params: VerbParams) => void,
  onConnection: (connected: boolean) => void,
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
      | undefined
    if (!message || typeof message !== 'object') return

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
