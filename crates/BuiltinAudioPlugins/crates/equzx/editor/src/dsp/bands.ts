import {
  bandpass,
  butterworthQs,
  magnitudeAt,
  type ResponseGrid,
  highShelf,
  highpass,
  lowShelf,
  lowpass,
  magnitude,
  notch,
  peaking,
  type Coeffs,
} from './biquad'
import { BAND_COLORS } from '../theme'

export type BandType =
  | 'lowcut'
  | 'lowshelf'
  | 'bell'
  | 'notch'
  | 'bandpass'
  | 'highshelf'
  | 'highcut'

/**
 * Which part of the stereo image a band acts on. Mid is (L+R)/2, side is (L-R)/2;
 * a 'stereo' band is simply the same filter applied to both.
 */
export type BandChannel = 'stereo' | 'mid' | 'side'

export const BAND_CHANNELS: { value: BandChannel; label: string; short: string }[] = [
  { value: 'stereo', label: 'Stereo', short: 'ST' },
  { value: 'mid', label: 'Mid', short: 'M' },
  { value: 'side', label: 'Side', short: 'S' },
]

export interface Band {
  id: number
  type: BandType
  channel: BandChannel
  freq: number
  /** dB, ignored by cut/notch/bandpass types. */
  gain: number
  q: number
  /** dB/oct for cut types. Only even filter orders, so multiples of 12. */
  slope: number
  enabled: boolean

  // --- dynamic section (only meaningful for gain-bearing types) ---
  /** When on, the band's gain moves by up to `dynRange` as the signal crosses `threshold`. */
  dynamic: boolean
  /** 'above' ducks/boosts once the band's level exceeds the threshold; 'below' inverts that. */
  dynMode: 'above' | 'below'
  /** Signed dB the band travels at full engagement. Negative ducks, positive boosts. */
  dynRange: number
  /** dBFS level, measured on the band-filtered input. */
  threshold: number
  attack: number
  release: number
}

/** dB past the threshold at which the band reaches its full range — a soft knee. */
export const DYN_KNEE_DB = 6

export const MAX_BANDS = 24

export const SLOPES = [12, 24, 36, 48, 72, 96] as const

export const BAND_TYPES: { value: BandType; label: string; glyph: string }[] = [
  { value: 'lowcut', label: 'Low Cut', glyph: 'M2 14 L8 14 Q13 14 13 3 L18 3' },
  { value: 'lowshelf', label: 'Low Shelf', glyph: 'M2 4 L7 4 Q10 4 10 9 L13 13 L18 13' },
  { value: 'bell', label: 'Bell', glyph: 'M2 13 Q7 13 10 4 Q13 13 18 13' },
  { value: 'notch', label: 'Notch', glyph: 'M2 4 Q9 4 10 14 Q11 4 18 4' },
  { value: 'bandpass', label: 'Band Pass', glyph: 'M2 14 Q9 14 10 4 Q11 14 18 14' },
  { value: 'highshelf', label: 'High Shelf', glyph: 'M2 13 L7 13 L10 9 Q10 4 13 4 L18 4' },
  { value: 'highcut', label: 'High Cut', glyph: 'M2 3 L7 3 Q12 3 12 14 L18 14' },
]

export const USES_GAIN: Record<BandType, boolean> = {
  lowcut: false,
  lowshelf: true,
  bell: true,
  notch: false,
  bandpass: false,
  highshelf: true,
  highcut: false,
}

/**
 * Shelves are fixed at S = 1: Web Audio's lowshelf/highshelf ignore Q, so exposing
 * it would be a control that changes nothing.
 */
export const USES_Q: Record<BandType, boolean> = {
  lowcut: false,
  lowshelf: false,
  bell: true,
  notch: true,
  bandpass: true,
  highshelf: false,
  highcut: false,
}

export const IS_CUT: Record<BandType, boolean> = {
  lowcut: true,
  lowshelf: false,
  bell: false,
  notch: false,
  bandpass: false,
  highshelf: false,
  highcut: true,
}

/** Per-band accents, cycled by band index. Defined with the rest of the palette. */
export { BAND_COLORS }

export function bandColor(index: number): string {
  return BAND_COLORS[index % BAND_COLORS.length]
}

/**
 * Expand one band into the biquad sections that realize it. Cut bands become a
 * Butterworth cascade; everything else is a single section.
 */
export function bandSections(band: Band, sr: number): Coeffs[] {
  const f = clampFreq(band.freq, sr)
  switch (band.type) {
    case 'bell':
      return [peaking(f, band.q, band.gain, sr)]
    case 'lowshelf':
      return [lowShelf(f, band.gain, sr)]
    case 'highshelf':
      return [highShelf(f, band.gain, sr)]
    case 'notch':
      return [notch(f, band.q, sr)]
    case 'bandpass':
      return [bandpass(f, band.q, sr)]
    case 'lowcut':
      return butterworthQs(band.slope / 6).map((q) => highpass(f, q, sr))
    case 'highcut':
      return butterworthQs(band.slope / 6).map((q) => lowpass(f, q, sr))
  }
}

function clampFreq(f: number, sr: number): number {
  return Math.min(Math.max(f, 10), sr / 2 - 1)
}

/** Combined dB response of a set of sections at one frequency. */
export function sectionsDb(sections: Coeffs[], f: number, sr: number): number {
  let mag = 1
  for (const s of sections) mag *= magnitude(s, f, sr)
  return 20 * Math.log10(Math.max(mag, 1e-7))
}

/** Combined dB response of a set of sections at grid point `i`. */
export function sectionsDbAt(sections: Coeffs[], grid: ResponseGrid, i: number): number {
  let mag = 1
  for (const s of sections) mag *= magnitudeAt(s, grid, i)
  return 20 * Math.log10(Math.max(mag, 1e-7))
}

export interface CompiledBand {
  band: Band
  index: number
  sections: Coeffs[]
}

export function compile(bands: Band[], sr: number): CompiledBand[] {
  return bands.map((band, index) => ({ band, index, sections: bandSections(band, sr) }))
}

/** Sum of all enabled bands, in dB, at one frequency. */
export function totalDb(compiled: CompiledBand[], f: number, sr: number): number {
  let db = 0
  for (const c of compiled) {
    if (!c.band.enabled) continue
    db += sectionsDb(c.sections, f, sr)
  }
  return db
}

let nextId = 1
/**
 * Ids are engine-internal identity — the audio graph keys its filter nodes by them —
 * so one is always issued here and a caller-supplied `id` is deliberately ignored.
 * That keeps a cloned band (A/B slots, presets) from aliasing the band it came from.
 */
export function makeBand(partial: Partial<Band> = {}): Band {
  return {
    type: 'bell',
    channel: 'stereo',
    freq: 1000,
    gain: 0,
    q: 1,
    slope: 24,
    enabled: true,
    dynamic: false,
    dynMode: 'above',
    dynRange: -6,
    threshold: -24,
    attack: 20,
    release: 200,
    ...partial,
    id: nextId++,
  }
}

/** The EQ opens flat — bands are created by clicking the display. */
export function defaultBands(): Band[] {
  return []
}

/** Which slice of the stereo image the display is showing. */
export type ChannelView = 'all' | 'mid' | 'side'

/** Does a band act on the channel currently being viewed? */
export function bandInView(band: Band, view: ChannelView): boolean {
  return view === 'all' || band.channel === 'stereo' || band.channel === view
}

/** Dynamics move the band's gain, so they only apply where gain means something. */
export function canBeDynamic(type: BandType): boolean {
  return USES_GAIN[type]
}
