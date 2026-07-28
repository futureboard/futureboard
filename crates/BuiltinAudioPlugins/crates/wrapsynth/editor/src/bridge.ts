export const BRIDGE_PROTOCOL_VERSION = 1
export const PLUGIN_ID = 'wrapsynth'

export type Waveform = 'saw' | 'square' | 'triangle' | 'sine'

export type SynthParams = {
  power: boolean
  oscAWave: Waveform
  oscAPosition: number
  oscALevel: number
  oscBWave: Waveform
  oscBPosition: number
  oscBLevel: number
  oscBSemitones: number
  oscBDetuneCents: number
  unison: number
  unisonDetuneCents: number
  stereoWidth: number
  subLevel: number
  noiseLevel: number
  cutoffHz: number
  resonance: number
  filterDrive: number
  attackMs: number
  decayMs: number
  sustain: number
  releaseMs: number
  masterDb: number
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
    // Standalone Vite preview intentionally has no native bridge.
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

function parseParams(state: unknown): SynthParams | null {
  if (!state || typeof state !== 'object') return null
  const candidate = 'params' in state ? (state as { params?: unknown }).params : state
  if (!candidate || typeof candidate !== 'object') return null
  const params = candidate as Partial<SynthParams>
  if (
    typeof params.power !== 'boolean' ||
    typeof params.oscAPosition !== 'number' ||
    typeof params.cutoffHz !== 'number' ||
    typeof params.attackMs !== 'number'
  ) return null
  return params as SynthParams
}

export function connectBridge(
  onParams: (params: SynthParams) => void,
  onConnection: (connected: boolean) => void,
) {
  post({
    type: 'futureboard.bridgeReady',
    protocolVersion: BRIDGE_PROTOCOL_VERSION,
    bridgeVersion: BRIDGE_PROTOCOL_VERSION,
    pluginId: PLUGIN_ID,
  })

  const listener = (event: MessageEvent) => {
    const message = event.data as SelectInstanceMessage | InstanceRemovedMessage | undefined
    if (!message || typeof message !== 'object') return
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
