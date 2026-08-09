export const BRIDGE_PROTOCOL_VERSION = 1
export const PLUGIN_ID = 'mixstation'

export type MixStationParams = {
  power: boolean
  inputTrimDb: number
  filtersEnabled: boolean
  hpfHz: number
  lpfHz: number
  eqEnabled: boolean
  lowGainDb: number
  lowMidFreqHz: number
  lowMidGainDb: number
  highMidFreqHz: number
  highMidGainDb: number
  highGainDb: number
  compEnabled: boolean
  compThresholdDb: number
  compRatio: number
  compAttackMs: number
  compReleaseMs: number
  compMakeupDb: number
  satEnabled: boolean
  satDrivePct: number
  satCharacterPct: number
  widthEnabled: boolean
  widthPct: number
  outputTrimDb: number
  limiterEnabled: boolean
  limiterCeilingDb: number
  limiterReleaseMs: number
  slot1Module: number
  slot2Module: number
  slot3Module: number
  slot4Module: number
  slot5Module: number
  slot6Module: number
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

export const defaults: MixStationParams = {
  power: true,
  inputTrimDb: 0,
  filtersEnabled: false,
  hpfHz: 30,
  lpfHz: 20_000,
  eqEnabled: false,
  lowGainDb: 0,
  lowMidFreqHz: 400,
  lowMidGainDb: 0,
  highMidFreqHz: 2_500,
  highMidGainDb: 0,
  highGainDb: 0,
  compEnabled: false,
  compThresholdDb: -18,
  compRatio: 4,
  compAttackMs: 10,
  compReleaseMs: 120,
  compMakeupDb: 0,
  satEnabled: false,
  satDrivePct: 0,
  satCharacterPct: 50,
  widthEnabled: false,
  widthPct: 100,
  outputTrimDb: 0,
  limiterEnabled: false,
  limiterCeilingDb: -0.3,
  limiterReleaseMs: 100,
  slot1Module: 0,
  slot2Module: 0,
  slot3Module: 0,
  slot4Module: 0,
  slot5Module: 0,
  slot6Module: 0,
}

export const BOOLEAN_KEYS = [
  'power',
  'filtersEnabled',
  'eqEnabled',
  'compEnabled',
  'satEnabled',
  'widthEnabled',
  'limiterEnabled',
] as const satisfies readonly (keyof MixStationParams)[]

export const NUMERIC_KEYS = [
  'inputTrimDb',
  'hpfHz',
  'lpfHz',
  'lowGainDb',
  'lowMidFreqHz',
  'lowMidGainDb',
  'highMidFreqHz',
  'highMidGainDb',
  'highGainDb',
  'compThresholdDb',
  'compRatio',
  'compAttackMs',
  'compReleaseMs',
  'compMakeupDb',
  'satDrivePct',
  'satCharacterPct',
  'widthPct',
  'outputTrimDb',
  'limiterCeilingDb',
  'limiterReleaseMs',
  'slot1Module',
  'slot2Module',
  'slot3Module',
  'slot4Module',
  'slot5Module',
  'slot6Module',
] as const satisfies readonly (keyof MixStationParams)[]

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
  bindingGeneration: number
}

type MetersMessage = MeterFrame & {
  type: 'futureboard.meters'
  protocolVersion: number
  instanceId: string
  bindingGeneration: number
}

let binding: Binding | null = null
let stateRevision = -1
let latestBindingGeneration = -1
let latestGenerationRemoved = false
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

export function makeSetParamsMessage(
  target: Binding,
  params: readonly { id: string; value: number }[],
) {
  return {
    type: 'futureboard.setParams' as const,
    protocolVersion: BRIDGE_PROTOCOL_VERSION,
    ...target,
    params,
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
  post(makeSetParamsMessage(binding, params))
}

export function postParam(id: keyof MixStationParams, value: number) {
  pending.set(id, value)
  if (scheduled) return
  scheduled = true
  requestAnimationFrame(flush)
}

/**
 * Ask the DAW for a workspace command (e.g. transport play/pause). Independent
 * of the param binding so Space still works before `selectInstance` arrives.
 * When the native shell already claimed the key, this page path does not run.
 */
export function postGlobalCommand(commandId: string): void {
  if (!commandId) return
  post({
    type: 'futureboard.globalCommand',
    commandId,
  })
}

export function parseParams(state: unknown): MixStationParams | null {
  if (!state || typeof state !== 'object') return null
  const candidate =
    'params' in state ? (state as { params?: unknown }).params : state
  if (!candidate || typeof candidate !== 'object') return null
  const params = { ...(candidate as Record<string, unknown>) }
  const legacySlots = [
    ['slot1Module', 'filtersEnabled', 1],
    ['slot2Module', 'eqEnabled', 2],
    ['slot3Module', 'compEnabled', 3],
    ['slot4Module', 'satEnabled', 4],
    ['slot5Module', 'widthEnabled', 5],
    ['slot6Module', 'limiterEnabled', 6],
  ] as const
  for (const [slot, enabled, module] of legacySlots) {
    if (params[slot] === undefined) {
      params[slot] = params[enabled] === true ? module : 0
    }
  }

  for (const key of BOOLEAN_KEYS) {
    if (typeof params[key] !== 'boolean') return null
  }
  for (const key of NUMERIC_KEYS) {
    if (typeof params[key] !== 'number' || !Number.isFinite(params[key])) return null
  }
  return params as MixStationParams
}

function isMeterFrame(message: MetersMessage) {
  return (
    Number.isFinite(message.inPeak) &&
    Number.isFinite(message.inRms) &&
    Number.isFinite(message.outPeak) &&
    Number.isFinite(message.outRms) &&
    Number.isFinite(message.gainReductionDb) &&
    typeof message.inClip === 'boolean' &&
    typeof message.outClip === 'boolean'
  )
}

export function connectBridge(
  onParams: (params: MixStationParams) => void,
  onConnection: (connected: boolean) => void,
  onMeters: (frame: MeterFrame) => void,
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
      if (
        binding?.instanceId !== message.instanceId ||
        message.bindingGeneration !== binding.bindingGeneration ||
        !isMeterFrame(message)
      ) {
        return
      }
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
      if (
        message.protocolVersion !== BRIDGE_PROTOCOL_VERSION ||
        message.pluginId !== PLUGIN_ID ||
        !Number.isSafeInteger(message.bindingGeneration) ||
        !Number.isSafeInteger(message.stateRevision) ||
        message.bindingGeneration < 0 ||
        message.stateRevision < 0
      ) {
        return
      }
      if (message.bindingGeneration < latestBindingGeneration) return
      if (
        message.bindingGeneration === latestBindingGeneration &&
        latestGenerationRemoved
      ) {
        return
      }
      if (binding) {
        if (
          message.bindingGeneration === binding.bindingGeneration &&
          (message.instanceId !== binding.instanceId ||
            message.stateRevision < stateRevision)
        ) {
          return
        }
      }
      latestBindingGeneration = message.bindingGeneration
      latestGenerationRemoved = false
      binding = {
        pluginId: message.pluginId,
        instanceId: message.instanceId,
        bindingGeneration: message.bindingGeneration,
      }
      stateRevision = message.stateRevision
      pending.clear()
      const params = parseParams(message.state)
      onParams(params ?? defaults)
      onConnection(true)
      post({
        type: 'futureboard.instanceReady',
        protocolVersion: BRIDGE_PROTOCOL_VERSION,
        ...binding,
        stateRevision: message.stateRevision,
      })
    } else if (
      message.type === 'futureboard.instanceRemoved' &&
      binding?.instanceId === message.instanceId &&
      message.bindingGeneration === latestBindingGeneration
    ) {
      binding = null
      stateRevision = -1
      latestGenerationRemoved = true
      pending.clear()
      onConnection(false)
    }
  }

  window.addEventListener('message', listener)
  return () => {
    window.removeEventListener('message', listener)
    binding = null
    stateRevision = -1
    latestBindingGeneration = -1
    latestGenerationRemoved = false
    pending.clear()
  }
}
