/**
 * Native bridge for the Z-Comp editor.
 *
 * Speaks the shared built-in wire contract plus `futureboard.meters` telemetry
 * for the gain-reduction and I/O meters.
 */

export const BRIDGE_PROTOCOL_VERSION = 1
export const PLUGIN_ID = 'zcomp'

export const MODELS = ['comp2500', 'distressor', 'avalon', 'ssl'] as const
export type CompModel = (typeof MODELS)[number]

export type ZcompParams = {
  power: boolean
  model: CompModel
  thresholdDb: number
  ratio: number
  attackMs: number
  releaseMs: number
  kneeDb: number
  makeupDb: number
  mix: number
  sidechainHpfHz: number
  stereoLink: number
  color: number
  autoRelease: boolean
}

export type MeterFrame = {
  inPeak: number
  inRms: number
  outPeak: number
  outRms: number
  gainReductionDb: number
  inClip: boolean
  outClip: boolean
}

export const MODEL_WIRE: Record<CompModel, number> = {
  comp2500: 0,
  distressor: 1,
  avalon: 2,
  ssl: 3,
}

export const MODEL_LABEL: Record<CompModel, string> = {
  comp2500: '2500',
  distressor: 'Distress',
  avalon: 'Avalon',
  ssl: 'SSL',
}

export const defaults: ZcompParams = {
  power: true,
  model: 'ssl',
  thresholdDb: -18,
  ratio: 4,
  attackMs: 10,
  releaseMs: 100,
  kneeDb: 6,
  makeupDb: 0,
  mix: 100,
  sidechainHpfHz: 60,
  stereoLink: 100,
  color: 18,
  autoRelease: true,
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
    // Standalone Vite preview has no native bridge.
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

const NUMERIC_KEYS = [
  'thresholdDb',
  'ratio',
  'attackMs',
  'releaseMs',
  'kneeDb',
  'makeupDb',
  'mix',
  'sidechainHpfHz',
  'stereoLink',
  'color',
] as const

function parseParams(state: unknown): ZcompParams | null {
  if (!state || typeof state !== 'object') return null
  const candidate =
    'params' in state ? (state as { params?: unknown }).params : state
  if (!candidate || typeof candidate !== 'object') return null
  const params = candidate as Record<string, unknown>

  if (typeof params.power !== 'boolean') return null
  if (typeof params.autoRelease !== 'boolean') return null
  if (!MODELS.includes(params.model as CompModel)) return null
  for (const key of NUMERIC_KEYS) {
    const value = params[key]
    if (typeof value !== 'number' || !Number.isFinite(value)) return null
  }
  // Clamp like `ipc::sanitize_params` so a hand-edited blob cannot drive
  // knobs outside the ranges the DSP accepts.
  const raw = params as unknown as ZcompParams
  return {
    ...raw,
    thresholdDb: Math.min(0, Math.max(-60, raw.thresholdDb)),
    ratio: Math.min(20, Math.max(1, raw.ratio)),
    attackMs: Math.min(120, Math.max(0.01, raw.attackMs)),
    releaseMs: Math.min(2500, Math.max(10, raw.releaseMs)),
    kneeDb: Math.min(24, Math.max(0, raw.kneeDb)),
    makeupDb: Math.min(24, Math.max(-24, raw.makeupDb)),
    mix: Math.min(100, Math.max(0, raw.mix)),
    sidechainHpfHz: Math.min(500, Math.max(20, raw.sidechainHpfHz)),
    stereoLink: Math.min(100, Math.max(0, raw.stereoLink)),
    color: Math.min(100, Math.max(0, raw.color)),
  }
}

export function connectBridge(
  onParams: (params: ZcompParams) => void,
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
