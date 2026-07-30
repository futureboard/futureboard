/**
 * Editor-side mirror of `zcomp::ipc` / `descriptor()` ranges and defaults.
 * Rust remains authoritative — this only keeps knobs and sanitization honest.
 */

import { defaults, type ZcompParams } from './bridge'

export type ParamSpec = {
  id: keyof ZcompParams
  label: string
  min: number
  max: number
  step: number
  unit: string
  defaultValue: number
  /** Engraved end-of-travel legends on the knob collar. */
  scale?: readonly [string, string]
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
    scale: ['-60', '0'],
  },
  ratio: {
    id: 'ratio',
    label: 'Ratio',
    min: 1,
    max: 20,
    step: 0.1,
    unit: ':1',
    defaultValue: defaults.ratio,
    scale: ['1', '20'],
  },
  attackMs: {
    id: 'attackMs',
    label: 'Attack',
    min: 0.01,
    max: 120,
    step: 0.01,
    unit: 'ms',
    defaultValue: defaults.attackMs,
    scale: ['0.01', '120'],
  },
  releaseMs: {
    id: 'releaseMs',
    label: 'Release',
    min: 10,
    max: 2500,
    step: 1,
    unit: 'ms',
    defaultValue: defaults.releaseMs,
    scale: ['10', '2.5k'],
  },
  kneeDb: {
    id: 'kneeDb',
    label: 'Knee',
    min: 0,
    max: 24,
    step: 0.1,
    unit: 'dB',
    defaultValue: defaults.kneeDb,
    scale: ['0', '24'],
  },
  makeupDb: {
    id: 'makeupDb',
    label: 'Makeup',
    min: -24,
    max: 24,
    step: 0.1,
    unit: 'dB',
    defaultValue: defaults.makeupDb,
    scale: ['-24', '+24'],
  },
  mix: {
    id: 'mix',
    label: 'Mix',
    min: 0,
    max: 100,
    step: 1,
    unit: '%',
    defaultValue: defaults.mix,
    scale: ['0', '100'],
  },
  sidechainHpfHz: {
    id: 'sidechainHpfHz',
    label: 'SC HPF',
    min: 20,
    max: 500,
    step: 1,
    unit: 'Hz',
    defaultValue: defaults.sidechainHpfHz,
    scale: ['20', '500'],
  },
  stereoLink: {
    id: 'stereoLink',
    label: 'Link',
    min: 0,
    max: 100,
    step: 1,
    unit: '%',
    defaultValue: defaults.stereoLink,
    scale: ['0', '100'],
  },
  color: {
    id: 'color',
    label: 'Color',
    min: 0,
    max: 100,
    step: 1,
    unit: '%',
    defaultValue: defaults.color,
    scale: ['0', '100'],
  },
} as const satisfies Record<string, ParamSpec>

/** Matches `CompModel` / `model_coeffs` character in Rust — engraved, not marketing. */
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
