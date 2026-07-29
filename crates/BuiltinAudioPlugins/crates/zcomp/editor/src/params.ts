/**
 * Editor-side mirror of `zcomp::ipc` / `descriptor()` ranges and defaults.
 * Rust remains authoritative — this only keeps knobs and sanitization honest.
 */

import { defaults, type CompModel, type ZcompParams } from './bridge'

export type ParamSpec = {
  id: keyof ZcompParams
  label: string
  min: number
  max: number
  step: number
  unit: string
  defaultValue: number
}

export const PARAM_SPECS = {
  thresholdDb: {
    id: 'thresholdDb',
    label: 'Thresh',
    min: -60,
    max: 0,
    step: 0.1,
    unit: 'dB',
    defaultValue: defaults.thresholdDb,
  },
  ratio: {
    id: 'ratio',
    label: 'Ratio',
    min: 1,
    max: 20,
    step: 0.1,
    unit: ':1',
    defaultValue: defaults.ratio,
  },
  attackMs: {
    id: 'attackMs',
    label: 'Attack',
    min: 0.01,
    max: 120,
    step: 0.01,
    unit: 'ms',
    defaultValue: defaults.attackMs,
  },
  releaseMs: {
    id: 'releaseMs',
    label: 'Release',
    min: 10,
    max: 2500,
    step: 1,
    unit: 'ms',
    defaultValue: defaults.releaseMs,
  },
  kneeDb: {
    id: 'kneeDb',
    label: 'Knee',
    min: 0,
    max: 24,
    step: 0.1,
    unit: 'dB',
    defaultValue: defaults.kneeDb,
  },
  makeupDb: {
    id: 'makeupDb',
    label: 'Makeup',
    min: -24,
    max: 24,
    step: 0.1,
    unit: 'dB',
    defaultValue: defaults.makeupDb,
  },
  mix: {
    id: 'mix',
    label: 'Mix',
    min: 0,
    max: 100,
    step: 1,
    unit: '%',
    defaultValue: defaults.mix,
  },
  sidechainHpfHz: {
    id: 'sidechainHpfHz',
    label: 'SC HPF',
    min: 20,
    max: 500,
    step: 1,
    unit: 'Hz',
    defaultValue: defaults.sidechainHpfHz,
  },
  stereoLink: {
    id: 'stereoLink',
    label: 'Link',
    min: 0,
    max: 100,
    step: 1,
    unit: '%',
    defaultValue: defaults.stereoLink,
  },
  color: {
    id: 'color',
    label: 'Color',
    min: 0,
    max: 100,
    step: 1,
    unit: '%',
    defaultValue: defaults.color,
  },
} as const satisfies Record<string, ParamSpec>

/** Matches `CompModel` / `model_coeffs` character in Rust — engraved, not marketing. */
export const MODEL_CIRCUIT: Record<
  CompModel,
  { title: string; topology: string }
> = {
  comp2500: {
    title: '2500',
    topology: 'VCA feed-forward · soft dual knee · THD colour',
  },
  distressor: {
    title: 'Distress',
    topology: 'Aggressive detector · British grit · hard ratios',
  },
  avalon: {
    title: 'Avalon',
    topology: 'Class-A optical feedback · slow musical recovery',
  },
  ssl: {
    title: 'SSL',
    topology: 'Bus glue · soft knee · program auto-release',
  },
}

export function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value))
}

/** Same clamps as `ipc::sanitize_params`. */
export function sanitizeParams(params: ZcompParams): ZcompParams {
  return {
    ...params,
    thresholdDb: clamp(params.thresholdDb, -60, 0),
    ratio: clamp(params.ratio, 1, 20),
    attackMs: clamp(params.attackMs, 0.01, 120),
    releaseMs: clamp(params.releaseMs, 10, 2500),
    kneeDb: clamp(params.kneeDb, 0, 24),
    makeupDb: clamp(params.makeupDb, -24, 24),
    mix: clamp(params.mix, 0, 100),
    sidechainHpfHz: clamp(params.sidechainHpfHz, 20, 500),
    stereoLink: clamp(params.stereoLink, 0, 100),
    color: clamp(params.color, 0, 100),
  }
}

/**
 * SSL auto-release owns recovery timing in `model_coeffs` — the release knob
 * is ignored while that path is active. Mirror that in the faceplate.
 */
export function releaseIsProgrammed(params: ZcompParams) {
  return params.model === 'ssl' && params.autoRelease
}
