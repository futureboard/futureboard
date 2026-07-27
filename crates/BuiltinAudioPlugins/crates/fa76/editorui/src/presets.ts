/**
 * Factory presets for FA-76.
 *
 * These are editor-side starting points that push real parameter values through
 * the existing bridge. Rust still clamps and owns DSP state — presets never
 * invent controls or bypass the wire contract.
 */

import { postParam, type Fa76Params } from './bridge'
import { PARAMS, ratioToWire, type ParamId, type Ratio } from './params'

export const DEFAULT_PARAMS: Fa76Params = {
  power: true,
  ratio: 'r4',
  inputDb: PARAMS.inputDb.default,
  outputDb: PARAMS.outputDb.default,
  attackUs: PARAMS.attackUs.default,
  releaseMs: PARAMS.releaseMs.default,
  mix: PARAMS.mix.default,
  sidechainHpfHz: PARAMS.sidechainHpfHz.default,
}

export type FactoryPreset = {
  name: string
  params: Fa76Params
}

function preset(name: string, patch: Partial<Fa76Params>): FactoryPreset {
  return {
    name,
    params: { ...DEFAULT_PARAMS, ...patch, power: true },
  }
}

export const FACTORY_PRESETS: FactoryPreset[] = [
  preset('Default', {}),
  preset('Vocal Catch', {
    ratio: 'r4',
    inputDb: 22,
    outputDb: -14,
    attackUs: 80,
    releaseMs: 220,
    mix: 100,
    sidechainHpfHz: 120,
  }),
  preset('Drum Punch', {
    ratio: 'r8',
    inputDb: 24,
    outputDb: -16,
    attackUs: 20,
    releaseMs: 80,
    mix: 100,
    sidechainHpfHz: 80,
  }),
  preset('Bass Glue', {
    ratio: 'r4',
    inputDb: 20,
    outputDb: -12,
    attackUs: 200,
    releaseMs: 350,
    mix: 100,
    sidechainHpfHz: 40,
  }),
  preset('Bus Soft', {
    ratio: 'r4',
    inputDb: 14,
    outputDb: -8,
    attackUs: 400,
    releaseMs: 600,
    mix: 100,
    sidechainHpfHz: 90,
  }),
  preset('Limiting', {
    ratio: 'r20',
    inputDb: 26,
    outputDb: -18,
    attackUs: 20,
    releaseMs: 120,
    mix: 100,
    sidechainHpfHz: 60,
  }),
  preset('All Buttons', {
    ratio: 'all',
    inputDb: 28,
    outputDb: -20,
    attackUs: 20,
    releaseMs: 90,
    mix: 100,
    sidechainHpfHz: 100,
  }),
  preset('Parallel Smash', {
    ratio: 'all',
    inputDb: 30,
    outputDb: -18,
    attackUs: 20,
    releaseMs: 70,
    mix: 42,
    sidechainHpfHz: 150,
  }),
]

const NUMERIC_IDS = Object.keys(PARAMS) as ParamId[]

export function paramsMatch(left: Fa76Params, right: Fa76Params) {
  if (left.power !== right.power || left.ratio !== right.ratio) {
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

export function matchingPresetIndex(params: Fa76Params) {
  const index = FACTORY_PRESETS.findIndex((entry) =>
    paramsMatch(params, entry.params),
  )
  return index >= 0 ? index : null
}

/** Whole-state push; the bridge coalesces into one `setParams` per frame. */
export function postAllParams(params: Fa76Params) {
  postParam('power', params.power ? 1 : 0)
  postParam('ratio', ratioToWire(params.ratio as Ratio))
  for (const id of NUMERIC_IDS) {
    postParam(id, params[id])
  }
}
