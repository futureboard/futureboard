/**
 * Display-side mirror of the Rust DSP's transfer functions.
 *
 * Every curve the editor draws is computed from the authoritative parameter
 * values using the same formulas as `src/dsp.rs`, so the graphics describe the
 * processing that actually runs rather than an artist's impression of it. When
 * a coefficient or constant changes in Rust it must change here too; the values
 * below are annotated with their Rust counterparts.
 */

/**
 * The bridge does not carry the host sample rate, so curves are drawn at a
 * 48 kHz reference. Below ~15 kHz the difference against 44.1/96 kHz is far
 * under a pixel; the top octave shifts slightly and is not used for readout.
 */
export const DISPLAY_SAMPLE_RATE = 48_000

/** `dsp::BUTTERWORTH_4_Q` — pole Qs of the 24 dB/oct cascade. */
const BUTTERWORTH_4_Q = [0.541_196_1, 1.306_562_9] as const

/** `lib::HPF_OPEN_HZ` / `lib::LPF_OPEN_HZ` — parked here the cut leaves the path. */
export const HPF_OPEN_HZ = 20
export const LPF_OPEN_HZ = 20_000

/** `lib::apply_params` pins the shelves; only their gains are on the panel. */
export const LOW_SHELF_HZ = 100
export const HIGH_SHELF_HZ = 10_000

/** `lib::SATURATION_DRIVE_SCALE` — drive percent to waveshaper amount. */
export const SATURATION_DRIVE_SCALE = 0.06
/** `dsp::SATURATION_FLOOR` */
const SATURATION_FLOOR = 1.0e-4
/** `dsp::SATURATION_BIAS` */
const SATURATION_BIAS = 0.25

/** Fixed soft-knee width the strip compressor is built with in `lib::apply_params`. */
export const COMP_KNEE_DB = 6

export type Biquad = { b0: number; b1: number; b2: number; a1: number; a2: number }

const IDENTITY: Biquad = { b0: 1, b1: 0, b2: 0, a1: 0, a2: 0 }

/** `dsp::omega` */
function omega(sampleRate: number, frequency: number) {
  const clamped = Math.min(Math.max(frequency, 10), Math.max(sampleRate, 1) * 0.49)
  return (2 * Math.PI * clamped) / Math.max(sampleRate, 1)
}

function normalized(
  b0: number,
  b1: number,
  b2: number,
  a0: number,
  a1: number,
  a2: number,
): Biquad {
  const inv = 1 / a0
  return { b0: b0 * inv, b1: b1 * inv, b2: b2 * inv, a1: a1 * inv, a2: a2 * inv }
}

/** `dsp::Biquad::set_high_pass` */
export function highPass(sampleRate: number, frequency: number, q: number): Biquad {
  const w = omega(sampleRate, frequency)
  const cos = Math.cos(w)
  const alpha = Math.sin(w) / (2 * Math.max(q, 0.1))
  return normalized(
    (1 + cos) * 0.5,
    -(1 + cos),
    (1 + cos) * 0.5,
    1 + alpha,
    -2 * cos,
    1 - alpha,
  )
}

/** `dsp::Biquad::set_low_pass` */
export function lowPass(sampleRate: number, frequency: number, q: number): Biquad {
  const w = omega(sampleRate, frequency)
  const cos = Math.cos(w)
  const alpha = Math.sin(w) / (2 * Math.max(q, 0.1))
  return normalized((1 - cos) * 0.5, 1 - cos, (1 - cos) * 0.5, 1 + alpha, -2 * cos, 1 - alpha)
}

/** `dsp::Biquad::set_peak` */
export function peak(
  sampleRate: number,
  frequency: number,
  gainDb: number,
  q: number,
): Biquad {
  const a = Math.pow(10, gainDb / 40)
  const w = omega(sampleRate, frequency)
  const cos = Math.cos(w)
  const alpha = Math.sin(w) / (2 * Math.max(q, 0.1))
  return normalized(
    1 + alpha * a,
    -2 * cos,
    1 - alpha * a,
    1 + alpha / a,
    -2 * cos,
    1 - alpha / a,
  )
}

/** `dsp::Biquad::set_low_shelf` */
export function lowShelf(sampleRate: number, frequency: number, gainDb: number): Biquad {
  const a = Math.pow(10, gainDb / 40)
  const w = omega(sampleRate, frequency)
  const cos = Math.cos(w)
  const two = 2 * Math.sqrt(a) * (Math.sin(w) * Math.SQRT1_2)
  return normalized(
    a * (a + 1 - (a - 1) * cos + two),
    2 * a * (a - 1 - (a + 1) * cos),
    a * (a + 1 - (a - 1) * cos - two),
    a + 1 + (a - 1) * cos + two,
    -2 * (a - 1 + (a + 1) * cos),
    a + 1 + (a - 1) * cos - two,
  )
}

/** `dsp::Biquad::set_high_shelf` */
export function highShelf(sampleRate: number, frequency: number, gainDb: number): Biquad {
  const a = Math.pow(10, gainDb / 40)
  const w = omega(sampleRate, frequency)
  const cos = Math.cos(w)
  const two = 2 * Math.sqrt(a) * (Math.sin(w) * Math.SQRT1_2)
  return normalized(
    a * (a + 1 + (a - 1) * cos + two),
    -2 * a * (a - 1 + (a + 1) * cos),
    a * (a + 1 + (a - 1) * cos - two),
    a + 1 - (a - 1) * cos + two,
    2 * (a - 1 - (a + 1) * cos),
    a + 1 - (a - 1) * cos - two,
  )
}

/** `dsp::proportional_q` — the console-style Q that widens as gain backs off. */
export function proportionalQ(gainDb: number) {
  return 0.7 + Math.min(Math.abs(gainDb) / 18, 1) * 1.2
}

/** Magnitude of one section at `frequency`, in dB. */
export function magnitudeDb(section: Biquad, sampleRate: number, frequency: number) {
  const w = (2 * Math.PI * frequency) / sampleRate
  const cos1 = Math.cos(w)
  const sin1 = Math.sin(w)
  const cos2 = Math.cos(2 * w)
  const sin2 = Math.sin(2 * w)
  const nRe = section.b0 + section.b1 * cos1 + section.b2 * cos2
  const nIm = -(section.b1 * sin1 + section.b2 * sin2)
  const dRe = 1 + section.a1 * cos1 + section.a2 * cos2
  const dIm = -(section.a1 * sin1 + section.a2 * sin2)
  const num = Math.hypot(nRe, nIm)
  const den = Math.max(Math.hypot(dRe, dIm), 1e-12)
  return 20 * Math.log10(Math.max(num / den, 1e-9))
}

export function chainMagnitudeDb(
  sections: readonly Biquad[],
  sampleRate: number,
  frequency: number,
) {
  let db = 0
  for (const section of sections) db += magnitudeDb(section, sampleRate, frequency)
  return db
}

/**
 * The cut sections, exactly as `Filters::set_high_pass`/`set_low_pass` build
 * them — including dropping out of the path at the ends of their ranges, which
 * is why a strip with the filters parked open draws a flat line.
 */
export function filterSections(hpfHz: number, lpfHz: number, sampleRate: number): Biquad[] {
  const sections: Biquad[] = []
  if (hpfHz > HPF_OPEN_HZ) {
    for (const q of BUTTERWORTH_4_Q) sections.push(highPass(sampleRate, hpfHz, q))
  }
  if (lpfHz < LPF_OPEN_HZ) {
    for (const q of BUTTERWORTH_4_Q) sections.push(lowPass(sampleRate, lpfHz, q))
  }
  return sections.length > 0 ? sections : [IDENTITY]
}

/** The four EQ bands, matching the order and fixed corners in `lib::apply_params`. */
export function eqSections(
  values: {
    lowGainDb: number
    lowMidFreqHz: number
    lowMidGainDb: number
    highMidFreqHz: number
    highMidGainDb: number
    highGainDb: number
  },
  sampleRate: number,
): Biquad[] {
  return [
    lowShelf(sampleRate, LOW_SHELF_HZ, values.lowGainDb),
    peak(
      sampleRate,
      values.lowMidFreqHz,
      values.lowMidGainDb,
      proportionalQ(values.lowMidGainDb),
    ),
    peak(
      sampleRate,
      values.highMidFreqHz,
      values.highMidGainDb,
      proportionalQ(values.highMidGainDb),
    ),
    highShelf(sampleRate, HIGH_SHELF_HZ, values.highGainDb),
  ]
}

/** `dsp::StripCompressor::curve_gain_db` — reduction in dB, zero or negative. */
export function compressorGainDb(
  levelDb: number,
  thresholdDb: number,
  ratio: number,
  kneeDb = COMP_KNEE_DB,
) {
  const over = levelDb - thresholdDb
  const halfKnee = kneeDb * 0.5
  const safeRatio = Math.max(ratio, 1)
  if (over <= -halfKnee) return 0
  if (over >= halfKnee) return (1 / safeRatio - 1) * over
  const t = over + halfKnee
  return ((1 / safeRatio - 1) * (t * t)) / (2 * Math.max(kneeDb, 1e-6))
}

/** Output level for an input level, including make-up gain. */
export function compressorOutputDb(
  levelDb: number,
  thresholdDb: number,
  ratio: number,
  makeupDb: number,
) {
  return levelDb + compressorGainDb(levelDb, thresholdDb, ratio) + makeupDb
}

/** `dsp::saturation_bias` */
function saturationBias(drive: number, character: number) {
  return (
    (Math.min(Math.max(character, 0), 1) - 0.5) *
    2 *
    SATURATION_BIAS *
    (1 - Math.exp(-drive))
  )
}

/** `dsp::saturation_normalization` */
function saturationNormalization(drive: number, bias: number) {
  return Math.max(0.5 * (Math.tanh(drive + bias) - Math.tanh(bias - drive)), 1e-6)
}

/**
 * `dsp::saturate` — the level-matched soft curve. Character biases the
 * operating point so the two polarities bend at different rates, which is the
 * even-harmonic asymmetry the curve display is there to show.
 *
 * `drivePct` and `characterPct` are the panel values; the scaling to the
 * waveshaper's domain matches `lib::apply_params`.
 */
export function saturate(sample: number, drivePct: number, characterPct: number) {
  const drive = drivePct * SATURATION_DRIVE_SCALE
  if (drive <= SATURATION_FLOOR) return sample
  const character = characterPct * 0.01
  const bias = saturationBias(drive, character)
  const offset = Math.tanh(bias)
  return (Math.tanh(sample * drive + bias) - offset) / saturationNormalization(drive, bias)
}

/** `dsp::LIMITER_KNEE_DB` — reduction begins this far below the ceiling. */
export const LIMITER_KNEE_DB = 1.5

/**
 * `dsp::soft_over_db` — infinite-ratio reduction in dB for an input `overDb`
 * above the ceiling, eased through a quadratic knee.
 */
export function limiterSoftOverDb(overDb: number, kneeDb = LIMITER_KNEE_DB) {
  const halfKnee = kneeDb * 0.5
  if (overDb <= -halfKnee) return 0
  if (overDb >= halfKnee) return -overDb
  const t = overDb + halfKnee
  return -(t * t) / (2 * kneeDb)
}

/**
 * Limiter output level for an input level.
 *
 * `dsp::Limiter::process` takes the smaller of the knee gain and a hard
 * `ceiling / peak` division, so the ceiling is absolute — the curve flattens
 * exactly at it rather than creeping over.
 */
export function limiterOutputDb(levelDb: number, ceilingDb: number) {
  const kneeOut = levelDb + limiterSoftOverDb(levelDb - ceilingDb)
  return Math.min(kneeOut, ceilingDb)
}

/** `dsp::stereo_width` applied to a unit L/R pair, for the width read-out. */
export function stereoWidth(left: number, right: number, width: number) {
  const mid = (left + right) * 0.5
  const side = (left - right) * 0.5 * width
  return [mid + side, mid - side] as const
}
