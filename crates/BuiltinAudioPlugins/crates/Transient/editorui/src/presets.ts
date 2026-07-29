/**
 * Factory presets for Transient.
 *
 * These are editor-side starting points that push real parameter values
 * through the existing bridge. Rust still clamps and owns DSP state —
 * presets never invent controls or bypass the wire contract.
 */

import { postParam, type TransientParams } from './bridge'
import { PARAMS, type ParamId } from './params'

export const DEFAULT_PARAMS: TransientParams = {
  power: true,
  attack: PARAMS.attack.default,
  sustain: PARAMS.sustain.default,
  speed: PARAMS.speed.default,
  mix: PARAMS.mix.default,
  stereoLink: true,
}

export type FactoryPreset = {
  name: string
  params: TransientParams
}

function preset(
  name: string,
  patch: Partial<TransientParams>,
): FactoryPreset {
  return {
    name,
    params: { ...DEFAULT_PARAMS, ...patch, power: true },
  }
}

export const FACTORY_PRESETS: FactoryPreset[] = [
  preset('Default', {}),
  preset('Punch Up', {
    attack: 55,
    sustain: -15,
    speed: 60,
    mix: 100,
    stereoLink: true,
  }),
  preset('Snap Cut', {
    attack: -45,
    sustain: 20,
    speed: 70,
    mix: 100,
    stereoLink: true,
  }),
  preset('Body Boost', {
    attack: 10,
    sustain: 50,
    speed: 40,
    mix: 100,
    stereoLink: true,
  }),
  preset('Drum Gate', {
    attack: 35,
    sustain: -70,
    speed: 80,
    mix: 100,
    stereoLink: true,
  }),
]

const NUMERIC_IDS = Object.keys(PARAMS) as ParamId[]

export function paramsMatch(left: TransientParams, right: TransientParams) {
  if (left.power !== right.power || left.stereoLink !== right.stereoLink) {
    return false
  }
  return NUMERIC_IDS.every((id) => Math.abs(left[id] - right[id]) < 0.05)
}

export function matchingPresetIndex(params: TransientParams) {
  const index = FACTORY_PRESETS.findIndex((entry) =>
    paramsMatch(params, entry.params),
  )
  return index >= 0 ? index : null
}

/** Whole-state push; the bridge coalesces into one `setParams` per frame. */
export function postAllParams(params: TransientParams) {
  postParam('power', params.power ? 1 : 0)
  for (const id of NUMERIC_IDS) {
    postParam(id, params[id])
  }
  postParam('stereoLink', params.stereoLink ? 1 : 0)
}
