export const BRIDGE_PROTOCOL_VERSION = 1
export const PLUGIN_ID = 'equz8'

export type FilterType =
  | 'highpass'
  | 'lowshelf'
  | 'bell'
  | 'notch'
  | 'highshelf'
  | 'lowpass'

export type Band = {
  active: boolean
  bandType: FilterType
  freq: number
  gainDb: number
  q: number
}

/// `soloBand` value meaning "no band is soloed". Mirrors `ipc::SOLO_NONE`.
export const SOLO_NONE = -1

export type EqParams = {
  power: boolean
  outputDb: number
  mix: number
  bands: Band[]
  /// Index of the band being auditioned in isolation, or [`SOLO_NONE`].
  soloBand: number
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

/// One analyser frame from the plugin host, measured on the audio arriving at
/// this insert. `bins` are log-spaced across `minHz..maxHz` and quantised to
/// bytes: `0` is `floorDb`, `255` is `ceilDb`. The scale travels with the frame
/// rather than being duplicated as constants on this side.
export type SpectrumFrame = {
  minHz: number
  maxHz: number
  floorDb: number
  ceilDb: number
  bins: number[]
}

type SpectrumMessage = SpectrumFrame & {
  type: 'futureboard.spectrum'
  protocolVersion: number
  instanceId: string
}

/// Low-rate (~1 Hz) host telemetry. Only `sampleRate` is consumed here — the
/// response curve has to be drawn against the rate the audio actually runs at.
type HostStatusMessage = {
  type: 'futureboard.hostStatus'
  protocolVersion: number
  instanceId: string
  sampleRate: number
  blockSize: number
  latencySamples: number
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

function parseParams(state: unknown): EqParams | null {
  if (!state || typeof state !== 'object') return null
  const candidate =
    'params' in state ? (state as { params?: unknown }).params : state
  if (!candidate || typeof candidate !== 'object') return null
  const params = candidate as Partial<EqParams>
  if (!Array.isArray(params.bands) || params.bands.length !== 8) return null
  if (
    typeof params.power !== 'boolean' ||
    typeof params.outputDb !== 'number' ||
    typeof params.mix !== 'number'
  ) {
    return null
  }
  // Optional on purpose: state saved before band solo existed has no
  // `soloBand`, and rejecting it here would make those projects fail to load
  // rather than simply opening with solo off. Rust applies the same default.
  const soloBand =
    typeof params.soloBand === 'number' &&
    Number.isInteger(params.soloBand) &&
    params.soloBand >= 0 &&
    params.soloBand < params.bands.length
      ? params.soloBand
      : SOLO_NONE
  return { ...(params as EqParams), soloBand }
}

export function connectBridge(
  onParams: (params: EqParams) => void,
  onConnection: (connected: boolean) => void,
  onSpectrum?: (frame: SpectrumFrame) => void,
  onSampleRate?: (sampleRate: number) => void,
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
      | SpectrumMessage
      | HostStatusMessage
      | undefined
    if (!message || typeof message !== 'object') return

    // Highest-rate message by far (~30 Hz), so it is matched before the rest
    // and never reaches the binding bookkeeping below.
    if (message.type === 'futureboard.spectrum') {
      if (!onSpectrum || binding?.instanceId !== message.instanceId) return
      if (!Array.isArray(message.bins)) return
      onSpectrum({
        minHz: message.minHz,
        maxHz: message.maxHz,
        floorDb: message.floorDb,
        ceilDb: message.ceilDb,
        bins: message.bins,
      })
      return
    }

    // The response curve is computed against the rate the audio actually runs
    // at, so the picture matches what the DSP is doing rather than assuming
    // 48 kHz. It matters most at the top of the range, where the bilinear
    // transform warps hardest.
    if (message.type === 'futureboard.hostStatus') {
      if (binding?.instanceId !== message.instanceId) return
      if (typeof message.sampleRate === 'number' && message.sampleRate > 0) {
        onSampleRate?.(message.sampleRate)
      }
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
