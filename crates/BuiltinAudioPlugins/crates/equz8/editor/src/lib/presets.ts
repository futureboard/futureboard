import { SOLO_NONE, postParam, type Band, type EqParams } from '../bridge'
import { filterKind } from './eq'

function flatBand(
  bandType: Band['bandType'],
  freq: number,
  q: number,
): Band {
  return {
    active: false,
    bandType,
    freq,
    gainDb: 0,
    q,
    dynamic: false,
    thresholdDb: -24,
    rangeDb: 0,
    attackMs: 10,
    releaseMs: 100,
  }
}

/// Mirrors `default_params()` in the Rust DSP crate. Rust stays authoritative:
/// these values only seed the editor before the first `selectInstance`.
/// Every band starts inactive and flat — no curve on open.
export const DEFAULT_PARAMS: EqParams = {
  power: true,
  outputDb: 0,
  mix: 100,
  bands: [
    flatBand('highpass', 50, 0.7),
    flatBand('lowshelf', 120, 0.8),
    flatBand('bell', 250, 1.2),
    flatBand('bell', 750, 1.4),
    flatBand('bell', 1500, 1),
    flatBand('bell', 3500, 1.1),
    flatBand('highshelf', 8000, 0.8),
    flatBand('lowpass', 16000, 0.7),
  ],
  soloBand: SOLO_NONE,
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
        // Factory looks enable the full strip; Default stays all-off via
        // `DEFAULT_PARAMS` directly.
        active: true,
        dynamic: false,
        rangeDb: 0,
        ...(bands[index] ?? {}),
      })),
      // Solo is an audition state, never part of a preset: loading a preset
      // while listening to one band must not leave the EQ stuck in solo.
      soloBand: SOLO_NONE,
    },
  }
}

export function cloneParams(params: EqParams): EqParams {
  return { ...params, bands: params.bands.map((band) => ({ ...band })) }
}

export const FACTORY_PRESETS: FactoryPreset[] = [
  { name: 'Default', params: cloneParams(DEFAULT_PARAMS) },
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
      Math.abs(band.q - other.q) < 0.001 &&
      band.dynamic === other.dynamic &&
      Math.abs(band.thresholdDb - other.thresholdDb) < 0.01 &&
      Math.abs(band.rangeDb - other.rangeDb) < 0.01 &&
      Math.abs(band.attackMs - other.attackMs) < 0.01 &&
      Math.abs(band.releaseMs - other.releaseMs) < 0.01
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
  postParam('soloBand', params.soloBand)
  params.bands.forEach((band, index) => {
    const prefix = `band${index + 1}_`
    postParam(`${prefix}enabled`, band.active ? 1 : 0)
    postParam(`${prefix}type`, filterKind(band.bandType).wire)
    postParam(`${prefix}freq`, band.freq)
    postParam(`${prefix}gainDb`, band.gainDb)
    postParam(`${prefix}q`, band.q)
    postParam(`${prefix}dynEnabled`, band.dynamic ? 1 : 0)
    postParam(`${prefix}thresholdDb`, band.thresholdDb)
    postParam(`${prefix}rangeDb`, band.rangeDb)
    postParam(`${prefix}attackMs`, band.attackMs)
    postParam(`${prefix}releaseMs`, band.releaseMs)
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
  if (patch.dynamic !== undefined) {
    postParam(`${prefix}dynEnabled`, patch.dynamic ? 1 : 0)
  }
  if (patch.thresholdDb !== undefined) {
    postParam(`${prefix}thresholdDb`, patch.thresholdDb)
  }
  if (patch.rangeDb !== undefined) {
    postParam(`${prefix}rangeDb`, patch.rangeDb)
  }
  if (patch.attackMs !== undefined) {
    postParam(`${prefix}attackMs`, patch.attackMs)
  }
  if (patch.releaseMs !== undefined) {
    postParam(`${prefix}releaseMs`, patch.releaseMs)
  }
}
