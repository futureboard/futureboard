/**
 * Native bridge for the Transient editor.
 *
 * Same wire contract every built-in editor speaks, plus the telemetry the
 * waveform stage and meters run on: the host pushes `futureboard.meters` at
 * ~30 Hz for the bound instance, carrying the levels and the shaping amount
 * the DSP is actually applying. Nothing on this page invents a meter
 * reading.
 */

export const BRIDGE_PROTOCOL_VERSION = 1
export const PLUGIN_ID = 'transient'

/** The state blob `transient::ipc::TransientState` serializes. */
export type TransientParams = {
  power: boolean
  attack: number
  sustain: number
  speed: number
  mix: number
  stereoLink: boolean
}

/**
 * One telemetry frame from the plugin host, measured on this insert.
 *
 * `gainReductionDb` is the absolute magnitude of the applied dynamic gain
 * change in dB. Levels are linear 0..1.
 */
export type MeterFrame = {
  inPeak: number
  inRms: number
  outPeak: number
  outRms: number
  gainReductionDb: number
  inClip: boolean
  outClip: boolean
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

type MetersMessage = MeterFrame & {
  type: 'futureboard.meters'
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

const NUMERIC_KEYS = ['attack', 'sustain', 'speed', 'mix'] as const

function parseParams(state: unknown): TransientParams | null {
  if (!state || typeof state !== 'object') return null
  const candidate =
    'params' in state ? (state as { params?: unknown }).params : state
  if (!candidate || typeof candidate !== 'object') return null
  const params = candidate as Record<string, unknown>

  if (typeof params.power !== 'boolean') return null
  if (typeof params.stereoLink !== 'boolean') return null
  for (const key of NUMERIC_KEYS) {
    const value = params[key]
    if (typeof value !== 'number' || !Number.isFinite(value)) return null
  }
  return params as unknown as TransientParams
}

export function connectBridge(
  onParams: (params: TransientParams) => void,
  onConnection: (connected: boolean) => void,
  onMeters?: (frame: MeterFrame) => void,
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
      | MetersMessage
      | undefined
    if (!message || typeof message !== 'object') return

    // Highest-rate message by far (~30 Hz), so it is matched before the rest
    // and never reaches the binding bookkeeping below.
    if (message.type === 'futureboard.meters') {
      if (!onMeters || binding?.instanceId !== message.instanceId) return
      onMeters({
        inPeak: message.inPeak,
        inRms: message.inRms,
        outPeak: message.outPeak,
        outRms: message.outRms,
        gainReductionDb: message.gainReductionDb,
        inClip: message.inClip,
        outClip: message.outClip,
      })
      return
    }

    if (message.type === 'futureboard.selectInstance') {
      binding = {
        pluginId: message.pluginId,
        instanceId: message.instanceId,
        bindingGeneration: message.bindingGeneration,
      }
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
