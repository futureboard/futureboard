/**
 * Factory presets for Z-Comp.
 *
 * Editor-side starting points that push real values through the bridge. Rust
 * still clamps and owns DSP state.
 */

import {
  MODEL_WIRE,
  defaults,
  postParam,
  type CompModel,
  type ZcompParams,
} from './bridge'

export type FactoryPreset = {
  name: string
  params: ZcompParams
}

function preset(name: string, patch: Partial<ZcompParams>): FactoryPreset {
  return {
    name,
    params: { ...defaults, ...patch, power: true },
  }
}

export const FACTORY_PRESETS: FactoryPreset[] = [
  preset('Default', {}),
  preset('Vocal Catch', {
    model: 'avalon',
    thresholdDb: -22,
    ratio: 3,
    attackMs: 12,
    releaseMs: 180,
    kneeDb: 8,
    makeupDb: 2,
    mix: 100,
    sidechainHpfHz: 120,
    stereoLink: 100,
    color: 22,
    autoRelease: true,
  }),
  preset('Drum Punch', {
    model: 'distressor',
    thresholdDb: -16,
    ratio: 6,
    attackMs: 0.8,
    releaseMs: 80,
    kneeDb: 2,
    makeupDb: 3,
    mix: 100,
    sidechainHpfHz: 80,
    stereoLink: 100,
    color: 45,
    autoRelease: false,
  }),
  preset('Bass Glue', {
    model: 'comp2500',
    thresholdDb: -20,
    ratio: 4,
    attackMs: 25,
    releaseMs: 280,
    kneeDb: 6,
    makeupDb: 1.5,
    mix: 100,
    sidechainHpfHz: 40,
    stereoLink: 100,
    color: 28,
    autoRelease: true,
  }),
  preset('Bus Soft', {
    model: 'ssl',
    thresholdDb: -14,
    ratio: 2.5,
    attackMs: 18,
    releaseMs: 420,
    kneeDb: 10,
    makeupDb: 0.5,
    mix: 100,
    sidechainHpfHz: 90,
    stereoLink: 100,
    color: 12,
    autoRelease: true,
  }),
  preset('Mix Bus Glue', {
    model: 'ssl',
    thresholdDb: -12,
    ratio: 2,
    attackMs: 30,
    releaseMs: 600,
    kneeDb: 12,
    makeupDb: 0,
    mix: 100,
    sidechainHpfHz: 100,
    stereoLink: 100,
    color: 8,
    autoRelease: true,
  }),
  preset('Parallel Smash', {
    model: 'distressor',
    thresholdDb: -28,
    ratio: 12,
    attackMs: 0.2,
    releaseMs: 60,
    kneeDb: 0,
    makeupDb: 6,
    mix: 38,
    sidechainHpfHz: 150,
    stereoLink: 100,
    color: 70,
    autoRelease: false,
  }),
  preset('Optical Level', {
    model: 'avalon',
    thresholdDb: -24,
    ratio: 4,
    attackMs: 40,
    releaseMs: 800,
    kneeDb: 14,
    makeupDb: 3,
    mix: 100,
    sidechainHpfHz: 60,
    stereoLink: 100,
    color: 18,
    autoRelease: true,
  }),
]

const NUMERIC_KEYS: (keyof ZcompParams)[] = [
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
]

export function cloneParams(params: ZcompParams): ZcompParams {
  return { ...params }
}

export function paramsMatch(left: ZcompParams, right: ZcompParams) {
  if (
    left.power !== right.power ||
    left.model !== right.model ||
    left.autoRelease !== right.autoRelease
  ) {
    return false
  }
  return NUMERIC_KEYS.every((key) => {
    const a = left[key] as number
    const b = right[key] as number
    return Math.abs(a - b) < 0.05
  })
}

export function matchingPresetIndex(params: ZcompParams) {
  const index = FACTORY_PRESETS.findIndex((entry) =>
    paramsMatch(params, entry.params),
  )
  return index >= 0 ? index : null
}

export function postAllParams(params: ZcompParams) {
  postParam('power', params.power ? 1 : 0)
  postParam('model', MODEL_WIRE[params.model as CompModel])
  postParam('autoRelease', params.autoRelease ? 1 : 0)
  for (const key of NUMERIC_KEYS) {
    postParam(key, params[key] as number)
  }
}
