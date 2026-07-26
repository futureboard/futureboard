import type { Band, FilterType } from '../bridge'

export const MIN_FREQ = 20
export const MAX_FREQ = 20_000
export const GAIN_RANGE = 18
export const MIN_Q = 0.1
export const MAX_Q = 12
export const OUTPUT_MIN_DB = -24
export const OUTPUT_MAX_DB = 12
export const SAMPLE_RATE = 48_000

const FREQ_SPAN = Math.log10(MAX_FREQ / MIN_FREQ)

export type FilterKind = {
  type: FilterType
  label: string
  short: string
  wire: number
  glyph: string
}

export const FILTER_KINDS: FilterKind[] = [
  {
    type: 'highpass',
    label: 'High pass',
    short: 'HP',
    wire: 0,
    glyph: 'M3 17c8 0 6-12 15-12h11',
  },
  {
    type: 'lowshelf',
    label: 'Low shelf',
    short: 'LS',
    wire: 1,
    glyph: 'M3 15h8c7 0 6-10 13-10h5',
  },
  {
    type: 'bell',
    label: 'Bell',
    short: 'BELL',
    wire: 2,
    glyph: 'M3 15c7 0 7-10 13-10s6 10 13 10',
  },
  {
    type: 'notch',
    label: 'Notch',
    short: 'NOTCH',
    wire: 3,
    glyph: 'M3 5c7 0 7 10 13 10s6-10 13-10',
  },
  {
    type: 'highshelf',
    label: 'High shelf',
    short: 'HS',
    wire: 4,
    glyph: 'M3 5h5c7 0 6 10 13 10h8',
  },
  {
    type: 'lowpass',
    label: 'Low pass',
    short: 'LP',
    wire: 5,
    glyph: 'M3 5h10c9 0 7 12 16 12',
  },
]

/// Frequency-shaped band identity, cool at the bottom of the spectrum to warm
/// at the top, so a node's colour hints at where it sits before it is read.
export const BAND_COLORS = [
  '#5cb8e6',
  '#57c9bd',
  '#77c97f',
  '#c9c164',
  '#e0a463',
  '#e07f86',
  '#b184d6',
  '#8fa4e8',
] as const

export function filterKind(type: FilterType): FilterKind {
  return FILTER_KINDS.find((kind) => kind.type === type) ?? FILTER_KINDS[2]!
}

export function bandHasGain(type: FilterType) {
  return type === 'bell' || type === 'lowshelf' || type === 'highshelf'
}

export function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value))
}

export function frequencyToX(frequency: number, width: number) {
  return (Math.log10(frequency / MIN_FREQ) / FREQ_SPAN) * width
}

export function xToFrequency(x: number, width: number) {
  return MIN_FREQ * Math.pow(MAX_FREQ / MIN_FREQ, clamp(x / width, 0, 1))
}

export function gainToY(gain: number, height: number) {
  return ((GAIN_RANGE - gain) / (GAIN_RANGE * 2)) * height
}

export function yToGain(y: number, height: number) {
  return GAIN_RANGE - clamp(y / height, 0, 1) * GAIN_RANGE * 2
}

export function freqToProgress(frequency: number) {
  return Math.log10(frequency / MIN_FREQ) / FREQ_SPAN
}

export function progressToFreq(progress: number) {
  return MIN_FREQ * Math.pow(MAX_FREQ / MIN_FREQ, clamp(progress, 0, 1))
}

/// Decade ticks with every multiple drawn, so zoomed-out grids still read as
/// logarithmic instead of evenly spaced.
export const GRID_FREQUENCIES = [10, 100, 1_000, 10_000].flatMap((decade) =>
  [1, 2, 3, 4, 5, 6, 7, 8, 9]
    .map((step) => decade * step)
    .filter((value) => value >= MIN_FREQ && value <= MAX_FREQ),
)

export const LABELLED_FREQUENCIES = [30, 100, 300, 1_000, 3_000, 10_000]
export const GRID_GAINS = [12, 6, 0, -6, -12]

export function formatFrequency(value: number) {
  if (value >= 10_000) return `${(value / 1000).toFixed(1).replace(/\.0$/, '')}k`
  if (value >= 1_000) return `${(value / 1000).toFixed(2).replace(/\.?0+$/, '')}k`
  return `${Math.round(value)}`
}

export function formatGain(value: number) {
  const normalized = Math.abs(value) < 0.05 ? 0 : value
  return `${normalized > 0 ? '+' : ''}${normalized.toFixed(1)}`
}

export function formatQ(value: number) {
  return value.toFixed(2)
}

type Coefficients = {
  b0: number
  b1: number
  b2: number
  a1: number
  a2: number
}

export function bandCoefficients(band: Band): Coefficients {
  const w0 = (2 * Math.PI * clamp(band.freq, MIN_FREQ, MAX_FREQ)) / SAMPLE_RATE
  const cos = Math.cos(w0)
  const sin = Math.sin(w0)
  const alpha = sin / (2 * clamp(band.q, MIN_Q, MAX_Q))
  const a = Math.pow(10, band.gainDb / 40)
  let b0: number
  let b1: number
  let b2: number
  let a0: number
  let a1: number
  let a2: number

  switch (band.bandType) {
    case 'highpass':
      b0 = (1 + cos) / 2
      b1 = -(1 + cos)
      b2 = (1 + cos) / 2
      a0 = 1 + alpha
      a1 = -2 * cos
      a2 = 1 - alpha
      break
    case 'lowpass':
      b0 = (1 - cos) / 2
      b1 = 1 - cos
      b2 = (1 - cos) / 2
      a0 = 1 + alpha
      a1 = -2 * cos
      a2 = 1 - alpha
      break
    case 'notch':
      b0 = 1
      b1 = -2 * cos
      b2 = 1
      a0 = 1 + alpha
      a1 = -2 * cos
      a2 = 1 - alpha
      break
    case 'lowshelf': {
      const root = 2 * Math.sqrt(a) * alpha
      b0 = a * (a + 1 - (a - 1) * cos + root)
      b1 = 2 * a * (a - 1 - (a + 1) * cos)
      b2 = a * (a + 1 - (a - 1) * cos - root)
      a0 = a + 1 + (a - 1) * cos + root
      a1 = -2 * (a - 1 + (a + 1) * cos)
      a2 = a + 1 + (a - 1) * cos - root
      break
    }
    case 'highshelf': {
      const root = 2 * Math.sqrt(a) * alpha
      b0 = a * (a + 1 + (a - 1) * cos + root)
      b1 = -2 * a * (a - 1 + (a + 1) * cos)
      b2 = a * (a + 1 + (a - 1) * cos - root)
      a0 = a + 1 - (a - 1) * cos + root
      a1 = 2 * (a - 1 - (a + 1) * cos)
      a2 = a + 1 - (a - 1) * cos - root
      break
    }
    default:
      b0 = 1 + alpha * a
      b1 = -2 * cos
      b2 = 1 - alpha * a
      a0 = 1 + alpha / a
      a1 = -2 * cos
      a2 = 1 - alpha / a
  }

  return { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
}

function magnitudeDb(coeff: Coefficients, frequency: number) {
  const w = (2 * Math.PI * frequency) / SAMPLE_RATE
  const c1 = Math.cos(w)
  const s1 = Math.sin(w)
  const c2 = Math.cos(2 * w)
  const s2 = Math.sin(2 * w)
  const nr = coeff.b0 + coeff.b1 * c1 + coeff.b2 * c2
  const ni = -coeff.b1 * s1 - coeff.b2 * s2
  const dr = 1 + coeff.a1 * c1 + coeff.a2 * c2
  const di = -coeff.a1 * s1 - coeff.a2 * s2
  const magnitude = Math.sqrt((nr * nr + ni * ni) / (dr * dr + di * di))
  return 20 * Math.log10(Math.max(magnitude, 1e-6))
}

function sampleCount(width: number) {
  return Math.round(clamp(width / 3, 140, 420))
}

function tracePath(
  width: number,
  height: number,
  dbAt: (frequency: number) => number,
) {
  const samples = sampleCount(width)
  let path = ''
  for (let index = 0; index <= samples; index += 1) {
    const x = (index / samples) * width
    const db = clamp(dbAt(xToFrequency(x, width)), -GAIN_RANGE - 6, GAIN_RANGE + 6)
    path += `${index === 0 ? 'M' : 'L'}${x.toFixed(1)},${gainToY(db, height).toFixed(1)}`
  }
  return path
}

export function sumCurvePath(bands: Band[], width: number, height: number) {
  const active = bands
    .filter((band) => band.active)
    .map((band) => bandCoefficients(band))
  return tracePath(width, height, (frequency) =>
    active.reduce((sum, coeff) => sum + magnitudeDb(coeff, frequency), 0),
  )
}

export function bandCurvePath(band: Band, width: number, height: number) {
  const coeff = bandCoefficients(band)
  return tracePath(width, height, (frequency) => magnitudeDb(coeff, frequency))
}

export function sumDbAt(bands: Band[], frequency: number) {
  return bands
    .filter((band) => band.active)
    .reduce((sum, band) => sum + magnitudeDb(bandCoefficients(band), frequency), 0)
}
