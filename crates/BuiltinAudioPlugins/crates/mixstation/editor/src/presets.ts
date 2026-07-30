import {
  BOOLEAN_KEYS,
  NUMERIC_KEYS,
  defaults,
  postParam,
  type MixStationParams,
} from './bridge'

export type FactoryPreset = {
  name: string
  params: MixStationParams
}

function preset(name: string, patch: Partial<MixStationParams>): FactoryPreset {
  return { name, params: { ...defaults, ...patch, power: true } }
}

const LOADED_RACK = {
  filtersEnabled: true,
  eqEnabled: true,
  compEnabled: true,
  satEnabled: true,
  widthEnabled: true,
  limiterEnabled: true,
  slot1Module: 1,
  slot2Module: 2,
  slot3Module: 3,
  slot4Module: 4,
  slot5Module: 5,
  slot6Module: 6,
} as const

export const FACTORY_PRESETS: readonly FactoryPreset[] = [
  preset('Empty Rack', {}),
  preset('Mix Bus Polish', {
    ...LOADED_RACK,
    hpfHz: 28,
    lowGainDb: 0.8,
    lowMidFreqHz: 320,
    lowMidGainDb: -0.7,
    highMidFreqHz: 3_200,
    highMidGainDb: 0.5,
    highGainDb: 0.9,
    compThresholdDb: -15,
    compRatio: 2,
    compAttackMs: 30,
    compReleaseMs: 250,
    compMakeupDb: 0.7,
    satDrivePct: 12,
    satCharacterPct: 42,
    widthPct: 106,
    limiterCeilingDb: -0.5,
  }),
  preset('Drum Rack', {
    ...LOADED_RACK,
    hpfHz: 35,
    lowGainDb: 1.8,
    lowMidFreqHz: 520,
    lowMidGainDb: -1.5,
    highMidFreqHz: 4_200,
    highMidGainDb: 1.2,
    compThresholdDb: -21,
    compRatio: 4,
    compAttackMs: 18,
    compReleaseMs: 90,
    compMakeupDb: 1.5,
    satDrivePct: 24,
    satCharacterPct: 68,
    widthPct: 112,
  }),
  preset('Vocal Focus', {
    ...LOADED_RACK,
    hpfHz: 85,
    lowGainDb: -0.8,
    lowMidFreqHz: 280,
    lowMidGainDb: -1.7,
    highMidFreqHz: 3_600,
    highMidGainDb: 1.8,
    highGainDb: 1.2,
    compThresholdDb: -24,
    compRatio: 3,
    compAttackMs: 12,
    compReleaseMs: 160,
    compMakeupDb: 2,
    satDrivePct: 8,
    satCharacterPct: 34,
    widthPct: 100,
  }),
  preset('Low End Firm', {
    ...LOADED_RACK,
    hpfHz: 24,
    lowGainDb: 1.2,
    lowMidFreqHz: 180,
    lowMidGainDb: -0.8,
    highMidFreqHz: 2_400,
    highMidGainDb: 0.4,
    compThresholdDb: -20,
    compRatio: 3.5,
    compAttackMs: 35,
    compReleaseMs: 180,
    compMakeupDb: 1,
    satDrivePct: 18,
    satCharacterPct: 56,
    widthPct: 94,
  }),
  preset('Wide Master', {
    ...LOADED_RACK,
    lowGainDb: 0.4,
    highGainDb: 0.8,
    compThresholdDb: -12,
    compRatio: 1.6,
    compAttackMs: 50,
    compReleaseMs: 400,
    satDrivePct: 6,
    satCharacterPct: 30,
    widthPct: 118,
    limiterCeilingDb: -0.8,
    limiterReleaseMs: 180,
  }),
] as const

export function paramsMatch(left: MixStationParams, right: MixStationParams) {
  return (
    BOOLEAN_KEYS.every((key) => left[key] === right[key]) &&
    NUMERIC_KEYS.every((key) => Math.abs(left[key] - right[key]) < 0.05)
  )
}

export function matchingPresetIndex(params: MixStationParams) {
  const index = FACTORY_PRESETS.findIndex((entry) =>
    paramsMatch(params, entry.params),
  )
  return index >= 0 ? index : null
}

export function postAllParams(params: MixStationParams) {
  for (const key of BOOLEAN_KEYS) postParam(key, params[key] ? 1 : 0)
  for (const key of NUMERIC_KEYS) postParam(key, params[key])
}
