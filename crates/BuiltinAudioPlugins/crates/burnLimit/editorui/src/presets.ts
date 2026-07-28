/**
 * Factory presets for BurnLimit.
 */

import { postParam, type BurnLimitParams } from './bridge'
import { PARAMS, styleToWire, type ParamId, type Style } from './params'

export const DEFAULT_PARAMS: BurnLimitParams = {
  power: true,
  style: 'modern',
  gainDb: PARAMS.gainDb.default,
  ceilingDb: PARAMS.ceilingDb.default,
  releaseMs: PARAMS.releaseMs.default,
  lookaheadMs: PARAMS.lookaheadMs.default,
  truePeak: true,
  mix: PARAMS.mix.default,
  stereoLink: true,
}

export type FactoryPreset = {
  name: string
  params: BurnLimitParams
}

function preset(name: string, patch: Partial<BurnLimitParams>): FactoryPreset {
  return {
    name,
    params: { ...DEFAULT_PARAMS, ...patch, power: true },
  }
}

export const FACTORY_PRESETS: FactoryPreset[] = [
  preset('Default', {}),
  preset('Master Soft', {
    style: 'clean',
    gainDb: 3,
    ceilingDb: -0.5,
    releaseMs: 350,
    lookaheadMs: 4,
    truePeak: true,
    mix: 100,
  }),
  preset('Loud Punch', {
    style: 'punch',
    gainDb: 8,
    ceilingDb: -0.3,
    releaseMs: 80,
    lookaheadMs: 1.5,
    truePeak: true,
    mix: 100,
  }),
  preset('Broadcast Safe', {
    style: 'modern',
    gainDb: 4,
    ceilingDb: -1.0,
    releaseMs: 180,
    lookaheadMs: 5,
    truePeak: true,
    mix: 100,
  }),
  preset('Clip Heat', {
    style: 'clip',
    gainDb: 12,
    ceilingDb: -0.1,
    releaseMs: 40,
    lookaheadMs: 0.5,
    truePeak: false,
    mix: 100,
  }),
  preset('Parallel Glue', {
    style: 'punch',
    gainDb: 10,
    ceilingDb: -0.5,
    releaseMs: 120,
    lookaheadMs: 2,
    truePeak: true,
    mix: 45,
  }),
]

const NUMERIC_IDS = Object.keys(PARAMS) as ParamId[]

export function paramsMatch(left: BurnLimitParams, right: BurnLimitParams) {
  if (
    left.power !== right.power ||
    left.style !== right.style ||
    left.truePeak !== right.truePeak ||
    left.stereoLink !== right.stereoLink
  ) {
    return false
  }
  return NUMERIC_IDS.every((id) => {
    const tolerance =
      PARAMS[id].taper === 'exp'
        ? Math.max(Math.abs(right[id]) * 0.002, 0.05)
        : 0.05
    return Math.abs(left[id] - right[id]) < tolerance
  })
}

export function matchingPresetIndex(params: BurnLimitParams) {
  const index = FACTORY_PRESETS.findIndex((entry) =>
    paramsMatch(params, entry.params),
  )
  return index >= 0 ? index : null
}

export function postAllParams(params: BurnLimitParams) {
  postParam('power', params.power ? 1 : 0)
  postParam('style', styleToWire(params.style as Style))
  for (const id of NUMERIC_IDS) {
    postParam(id, params[id])
  }
  postParam('truePeak', params.truePeak ? 1 : 0)
  postParam('stereoLink', params.stereoLink ? 1 : 0)
}
