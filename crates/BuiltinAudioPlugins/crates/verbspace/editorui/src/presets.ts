/**
 * Factory presets for VerbSpace.
 *
 * These are editor-side starting points that push real parameter values through
 * the existing bridge. Rust still clamps and owns DSP state — presets never
 * invent controls or bypass the wire contract.
 */

import { postParam, type VerbParams } from './bridge'
import { PARAMS, modeToWire, type ParamId } from './params'

export const DEFAULT_PARAMS: VerbParams = {
  power: true,
  mode: 'hall',
  predelayMs: PARAMS.predelayMs.default,
  size: PARAMS.size.default,
  decaySec: PARAMS.decaySec.default,
  diffusion: PARAMS.diffusion.default,
  damping: PARAMS.damping.default,
  bassMult: PARAMS.bassMult.default,
  modDepth: PARAMS.modDepth.default,
  modRateHz: PARAMS.modRateHz.default,
  lowCutHz: PARAMS.lowCutHz.default,
  highCutHz: PARAMS.highCutHz.default,
  width: PARAMS.width.default,
  mix: PARAMS.mix.default,
  outputDb: PARAMS.outputDb.default,
  freeze: false,
}

export type FactoryPreset = {
  name: string
  params: VerbParams
}

function preset(name: string, patch: Partial<VerbParams>): FactoryPreset {
  return {
    name,
    params: { ...DEFAULT_PARAMS, ...patch, power: true, freeze: false },
  }
}

export const FACTORY_PRESETS: FactoryPreset[] = [
  preset('Default', {}),
  preset('Tight Room', {
    mode: 'room',
    predelayMs: 8,
    size: 32,
    decaySec: 0.55,
    diffusion: 58,
    damping: 55,
    bassMult: 0.95,
    modDepth: 12,
    modRateHz: 0.45,
    lowCutHz: 120,
    highCutHz: 11000,
    width: 95,
    mix: 22,
  }),
  preset('Live Chamber', {
    mode: 'chamber',
    predelayMs: 18,
    size: 48,
    decaySec: 1.35,
    diffusion: 78,
    damping: 42,
    bassMult: 1.05,
    modDepth: 18,
    modRateHz: 0.55,
    lowCutHz: 95,
    highCutHz: 12000,
    width: 105,
    mix: 26,
  }),
  preset('Concert Hall', {
    mode: 'hall',
    predelayMs: 28,
    size: 82,
    decaySec: 3.8,
    diffusion: 80,
    damping: 38,
    bassMult: 1.25,
    modDepth: 32,
    modRateHz: 0.42,
    lowCutHz: 70,
    highCutHz: 9000,
    width: 125,
    mix: 30,
  }),
  preset('Bright Plate', {
    mode: 'plate',
    predelayMs: 12,
    size: 55,
    decaySec: 1.9,
    diffusion: 88,
    damping: 22,
    bassMult: 0.9,
    modDepth: 14,
    modRateHz: 0.7,
    lowCutHz: 140,
    highCutHz: 16000,
    width: 115,
    mix: 28,
  }),
  preset('Dark Hall', {
    mode: 'hall',
    predelayMs: 35,
    size: 74,
    decaySec: 4.6,
    diffusion: 70,
    damping: 68,
    bassMult: 1.45,
    modDepth: 28,
    modRateHz: 0.35,
    lowCutHz: 55,
    highCutHz: 6200,
    width: 118,
    mix: 32,
  }),
  preset('Vocal Ambience', {
    mode: 'ambience',
    predelayMs: 14,
    size: 38,
    decaySec: 0.95,
    diffusion: 65,
    damping: 40,
    bassMult: 0.85,
    modDepth: 20,
    modRateHz: 0.8,
    lowCutHz: 160,
    highCutHz: 14000,
    width: 100,
    mix: 18,
  }),
  preset('Drum Room', {
    mode: 'room',
    predelayMs: 4,
    size: 40,
    decaySec: 0.72,
    diffusion: 50,
    damping: 48,
    bassMult: 1.15,
    modDepth: 8,
    modRateHz: 0.35,
    lowCutHz: 80,
    highCutHz: 10500,
    width: 90,
    mix: 24,
  }),
]

const NUMERIC_IDS = Object.keys(PARAMS) as ParamId[]

export function paramsMatch(left: VerbParams, right: VerbParams) {
  if (
    left.power !== right.power ||
    left.freeze !== right.freeze ||
    left.mode !== right.mode
  ) {
    return false
  }
  return NUMERIC_IDS.every(
    (id) => Math.abs(left[id] - right[id]) < (id === 'bassMult' ? 0.01 : 0.05),
  )
}

export function matchingPresetIndex(params: VerbParams) {
  const index = FACTORY_PRESETS.findIndex((entry) =>
    paramsMatch(params, entry.params),
  )
  return index >= 0 ? index : null
}

/** Whole-state push; the bridge coalesces into one `setParams` per frame. */
export function postAllParams(params: VerbParams) {
  postParam('power', params.power ? 1 : 0)
  postParam('mode', modeToWire(params.mode))
  for (const id of NUMERIC_IDS) {
    postParam(id, params[id])
  }
  postParam('freeze', params.freeze ? 1 : 0)
}
