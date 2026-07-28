/**
 * Factory presets for 67Clipper.
 *
 * These are editor-side starting points that push real parameter values
 * through the existing bridge. Rust still clamps and owns DSP state —
 * presets never invent controls or bypass the wire contract.
 */

import { postParam, type Clipper67Params } from './bridge'
import { PARAMS, modeToWire, type Mode, type ParamId } from './params'

export const DEFAULT_PARAMS: Clipper67Params = {
  power: true,
  mode: 'clip',
  thresholdDb: PARAMS.thresholdDb.default,
  shape: PARAMS.shape.default,
  ceilingDb: PARAMS.ceilingDb.default,
  mix: PARAMS.mix.default,
  stereoLink: true,
  dcFilter: true,
}

export type FactoryPreset = {
  name: string
  params: Clipper67Params
}

function preset(
  name: string,
  patch: Partial<Clipper67Params>,
): FactoryPreset {
  return {
    name,
    params: { ...DEFAULT_PARAMS, ...patch, power: true },
  }
}

export const FACTORY_PRESETS: FactoryPreset[] = [
  preset('Default', {}),
  preset('Soft Clip', {
    mode: 'clip',
    thresholdDb: -3,
    shape: 80,
    ceilingDb: -0.3,
    mix: 100,
    stereoLink: true,
    dcFilter: true,
  }),
  preset('Aggressive', {
    mode: 'clip',
    thresholdDb: -12,
    shape: 12,
    ceilingDb: -0.1,
    mix: 100,
    stereoLink: true,
    dcFilter: true,
  }),
  preset('Hybrid Glue', {
    mode: 'hybrid',
    thresholdDb: -8,
    shape: 60,
    ceilingDb: -0.3,
    mix: 100,
    stereoLink: true,
    dcFilter: true,
  }),
  preset('Brick Limit', {
    mode: 'limit',
    thresholdDb: -1,
    shape: 0,
    ceilingDb: -0.1,
    mix: 100,
    stereoLink: true,
    dcFilter: false,
  }),
]

const NUMERIC_IDS = Object.keys(PARAMS) as ParamId[]

export function paramsMatch(left: Clipper67Params, right: Clipper67Params) {
  if (
    left.power !== right.power ||
    left.mode !== right.mode ||
    left.stereoLink !== right.stereoLink ||
    left.dcFilter !== right.dcFilter
  ) {
    return false
  }
  return NUMERIC_IDS.every((id) => Math.abs(left[id] - right[id]) < 0.05)
}

export function matchingPresetIndex(params: Clipper67Params) {
  const index = FACTORY_PRESETS.findIndex((entry) =>
    paramsMatch(params, entry.params),
  )
  return index >= 0 ? index : null
}

/** Whole-state push; the bridge coalesces into one `setParams` per frame. */
export function postAllParams(params: Clipper67Params) {
  postParam('power', params.power ? 1 : 0)
  postParam('mode', modeToWire(params.mode as Mode))
  for (const id of NUMERIC_IDS) {
    postParam(id, params[id])
  }
  postParam('stereoLink', params.stereoLink ? 1 : 0)
  postParam('dcFilter', params.dcFilter ? 1 : 0)
}
