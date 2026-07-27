/**
 * Factory presets for EchoSpace.
 *
 * These are editor-side starting points that push real parameter values through
 * the existing bridge. Rust still clamps and owns DSP state — presets never
 * invent controls or bypass the wire contract.
 */

import { postParam, type EchoParams } from './bridge'
import { PARAMS, modeToWire, type ParamId } from './params'

export const DEFAULT_PARAMS: EchoParams = {
  power: true,
  mode: 'pingpong',
  timeMsL: PARAMS.timeMsL.default,
  timeMsR: PARAMS.timeMsR.default,
  feedback: PARAMS.feedback.default,
  crossFeedback: PARAMS.crossFeedback.default,
  lowCutHz: PARAMS.lowCutHz.default,
  highCutHz: PARAMS.highCutHz.default,
  saturation: PARAMS.saturation.default,
  mix: PARAMS.mix.default,
  outputDb: PARAMS.outputDb.default,
  freeze: false,
}

export type FactoryPreset = {
  name: string
  params: EchoParams
}

function preset(name: string, patch: Partial<EchoParams>): FactoryPreset {
  return {
    name,
    params: { ...DEFAULT_PARAMS, ...patch, power: true, freeze: false },
  }
}

export const FACTORY_PRESETS: FactoryPreset[] = [
  preset('Default', {}),
  preset('Slapback', {
    mode: 'mono',
    timeMsL: 96,
    timeMsR: 96,
    feedback: 8,
    crossFeedback: 0,
    lowCutHz: 220,
    highCutHz: 7000,
    saturation: 18,
    mix: 24,
  }),
  preset('Quarter Ping', {
    mode: 'pingpong',
    timeMsL: 500,
    timeMsR: 500,
    feedback: 38,
    crossFeedback: 80,
    lowCutHz: 160,
    highCutHz: 10000,
    saturation: 6,
    mix: 26,
  }),
  preset('Dotted Eighth', {
    mode: 'pingpong',
    timeMsL: 375,
    timeMsR: 250,
    feedback: 32,
    crossFeedback: 70,
    lowCutHz: 200,
    highCutHz: 11000,
    saturation: 4,
    mix: 22,
  }),
  preset('Tape Echo', {
    mode: 'stereo',
    timeMsL: 320,
    timeMsR: 340,
    feedback: 52,
    crossFeedback: 25,
    lowCutHz: 260,
    highCutHz: 4200,
    saturation: 62,
    mix: 30,
    outputDb: -1.5,
  }),
  preset('Dub Space', {
    mode: 'pingpong',
    timeMsL: 620,
    timeMsR: 930,
    feedback: 74,
    crossFeedback: 95,
    lowCutHz: 320,
    highCutHz: 3200,
    saturation: 48,
    mix: 38,
  }),
  preset('Wide Doubler', {
    mode: 'stereo',
    timeMsL: 28,
    timeMsR: 41,
    feedback: 0,
    crossFeedback: 0,
    lowCutHz: 140,
    highCutHz: 14000,
    saturation: 0,
    mix: 42,
  }),
  preset('Ambient Wash', {
    mode: 'pingpong',
    timeMsL: 1200,
    timeMsR: 1800,
    feedback: 82,
    crossFeedback: 88,
    lowCutHz: 240,
    highCutHz: 5200,
    saturation: 22,
    mix: 45,
    outputDb: -2,
  }),
]

const NUMERIC_IDS = Object.keys(PARAMS) as ParamId[]

export function paramsMatch(left: EchoParams, right: EchoParams) {
  if (
    left.power !== right.power ||
    left.freeze !== right.freeze ||
    left.mode !== right.mode
  ) {
    return false
  }
  // Delay times and cut frequencies travel through an exponential taper, so
  // compare them proportionally; a 0.05 absolute window would call 1200 ms and
  // 1800 ms a match on nothing but rounding.
  return NUMERIC_IDS.every((id) => {
    const tolerance =
      PARAMS[id].taper === 'exp'
        ? Math.max(Math.abs(right[id]) * 0.002, 0.05)
        : 0.05
    return Math.abs(left[id] - right[id]) < tolerance
  })
}

export function matchingPresetIndex(params: EchoParams) {
  const index = FACTORY_PRESETS.findIndex((entry) =>
    paramsMatch(params, entry.params),
  )
  return index >= 0 ? index : null
}

/** Whole-state push; the bridge coalesces into one `setParams` per frame. */
export function postAllParams(params: EchoParams) {
  postParam('power', params.power ? 1 : 0)
  postParam('mode', modeToWire(params.mode))
  for (const id of NUMERIC_IDS) {
    postParam(id, params[id])
  }
  postParam('freeze', params.freeze ? 1 : 0)
}
