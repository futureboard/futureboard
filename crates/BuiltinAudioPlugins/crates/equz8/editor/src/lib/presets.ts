import { postParam, type Band, type EqParams } from '../bridge'
import { filterKind } from './eq'

/// Mirrors `default_params()` in the Rust DSP crate. Rust stays authoritative:
/// these values only seed the editor before the first `selectInstance`.
export const DEFAULT_PARAMS: EqParams = {
  power: true,
  outputDb: 0,
  mix: 100,
  bands: [
    { active: true, bandType: 'highpass', freq: 50, gainDb: 0, q: 0.7 },
    { active: true, bandType: 'lowshelf', freq: 120, gainDb: 0, q: 0.8 },
    { active: true, bandType: 'bell', freq: 250, gainDb: 2.5, q: 1.2 },
    { active: true, bandType: 'bell', freq: 750, gainDb: -1.5, q: 1.4 },
    { active: true, bandType: 'bell', freq: 1500, gainDb: 1, q: 1 },
    { active: true, bandType: 'bell', freq: 3500, gainDb: 0, q: 1.1 },
    { active: true, bandType: 'highshelf', freq: 8000, gainDb: 1.5, q: 0.8 },
    { active: true, bandType: 'lowpass', freq: 16000, gainDb: 0, q: 0.7 },
  ],
}

export type FactoryPreset = {
  name: string
  params: EqParams
}

function preset(
  name: string,
  bands: Partial<Band>[],
  outputDb = 0,
  mix = 100,
): FactoryPreset {
  return {
    name,
    params: {
      power: true,
      outputDb,
      mix,
      bands: DEFAULT_PARAMS.bands.map((band, index) => ({
        ...band,
        ...(bands[index] ?? {}),
      })),
    },
  }
}

export const FACTORY_PRESETS: FactoryPreset[] = [
  preset('Default', []),
  preset('Low-End Control', [
    { freq: 32, q: 0.72 },
    { freq: 95, gainDb: -1.5, q: 0.8 },
    { freq: 220, gainDb: -2.2, q: 1.15 },
    { freq: 620, gainDb: 0, q: 1.2 },
    { freq: 1600, gainDb: 0.8, q: 1 },
    { freq: 3800, gainDb: 0, q: 1.1 },
    { freq: 9500, gainDb: 0.7, q: 0.75 },
    { freq: 19000, q: 0.7 },
  ]),
  preset('Vocal Clarity', [
    { freq: 70, q: 0.75 },
    { freq: 140, gainDb: -1.2, q: 0.8 },
    { freq: 320, gainDb: -2.4, q: 1.3 },
    { freq: 900, gainDb: -0.8, q: 1.5 },
    { freq: 2600, gainDb: 2.2, q: 1.1 },
    { freq: 4800, gainDb: -1.4, q: 2 },
    { freq: 11000, gainDb: 1.8, q: 0.72 },
    { freq: 19500, q: 0.7 },
  ]),
  preset(
    'Mix Bus Polish',
    [
      { freq: 24, q: 0.7 },
      { freq: 110, gainDb: 0.7, q: 0.75 },
      { freq: 280, gainDb: -0.8, q: 1 },
      { freq: 780, gainDb: -0.4, q: 1.2 },
      { freq: 2200, gainDb: 0.5, q: 0.9 },
      { freq: 5200, gainDb: -0.5, q: 1.4 },
      { freq: 12500, gainDb: 1.1, q: 0.68 },
      { freq: 20000, q: 0.7 },
    ],
    -0.3,
  ),
  preset('Tame Harshness', [
    { freq: 28, q: 0.7 },
    { freq: 120, gainDb: 0, q: 0.8 },
    { freq: 300, gainDb: -0.7, q: 1.1 },
    { freq: 1100, gainDb: 0.4, q: 1.2 },
    { freq: 3100, gainDb: -1.1, q: 1.8 },
    { freq: 6200, gainDb: -2, q: 2.4 },
    { freq: 10500, gainDb: -1, q: 0.8 },
    { freq: 19000, q: 0.72 },
  ]),
  preset('Air Lift', [
    { freq: 26, q: 0.7 },
    { freq: 130, gainDb: -0.6, q: 0.8 },
    { freq: 260, gainDb: 0, q: 1.2 },
    { freq: 700, gainDb: -0.5, q: 1.3 },
    { freq: 1800, gainDb: 0.6, q: 1 },
    { freq: 4200, gainDb: 1.2, q: 1.2 },
    { freq: 13500, gainDb: 2.6, q: 0.66 },
    { freq: 20000, q: 0.7 },
  ]),
]

export function cloneParams(params: EqParams): EqParams {
  return { ...params, bands: params.bands.map((band) => ({ ...band })) }
}

export function paramsMatch(left: EqParams, right: EqParams) {
  if (
    left.power !== right.power ||
    Math.abs(left.outputDb - right.outputDb) > 0.001 ||
    Math.abs(left.mix - right.mix) > 0.001
  ) {
    return false
  }
  return left.bands.every((band, index) => {
    const other = right.bands[index]
    return (
      other !== undefined &&
      band.active === other.active &&
      band.bandType === other.bandType &&
      Math.abs(band.freq - other.freq) < 0.01 &&
      Math.abs(band.gainDb - other.gainDb) < 0.01 &&
      Math.abs(band.q - other.q) < 0.001
    )
  })
}

export function matchingPresetIndex(params: EqParams) {
  const index = FACTORY_PRESETS.findIndex((entry) =>
    paramsMatch(params, entry.params),
  )
  return index >= 0 ? index : null
}

/// Whole-state push used when a preset replaces every value at once. The
/// bridge coalesces these into a single `setParams` message per frame.
export function postAllParams(params: EqParams) {
  postParam('power', params.power ? 1 : 0)
  postParam('outputDb', params.outputDb)
  postParam('mix', params.mix)
  params.bands.forEach((band, index) => {
    const prefix = `band${index + 1}_`
    postParam(`${prefix}enabled`, band.active ? 1 : 0)
    postParam(`${prefix}type`, filterKind(band.bandType).wire)
    postParam(`${prefix}freq`, band.freq)
    postParam(`${prefix}gainDb`, band.gainDb)
    postParam(`${prefix}q`, band.q)
  })
}

export function postBandPatch(index: number, patch: Partial<Band>) {
  const prefix = `band${index + 1}_`
  if (patch.active !== undefined) {
    postParam(`${prefix}enabled`, patch.active ? 1 : 0)
  }
  if (patch.bandType !== undefined) {
    postParam(`${prefix}type`, filterKind(patch.bandType).wire)
  }
  if (patch.freq !== undefined) postParam(`${prefix}freq`, patch.freq)
  if (patch.gainDb !== undefined) postParam(`${prefix}gainDb`, patch.gainDb)
  if (patch.q !== undefined) postParam(`${prefix}q`, patch.q)
}
