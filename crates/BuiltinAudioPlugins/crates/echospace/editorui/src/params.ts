/**
 * Editor-side mirror of EchoSpace's parameter contract.
 *
 * Rust owns the authority: `src/ipc.rs` defines the ids and their ranges, and
 * clamps every incoming value again. This table exists so the UI can lay out a
 * control, choose a taper, and format a readout — never to decide what a value
 * means. `params.test.ts` pins the ids and ranges against the Rust source.
 */

export type ParamId =
  | 'timeMsL'
  | 'timeMsR'
  | 'feedback'
  | 'crossFeedback'
  | 'lowCutHz'
  | 'highCutHz'
  | 'saturation'
  | 'mix'
  | 'outputDb'

/**
 * How the 0..1 control travel maps onto the value range.
 *
 * - `lin` — uniform.
 * - `exp` — constant ratio per unit travel; the only sane choice for a
 *   frequency or a delay time, where the useful resolution is at the bottom.
 * - a number — `value = min + (max - min) * t ** exponent`, for ranges that
 *   want more resolution low down without being fully logarithmic.
 */
type Taper = 'lin' | 'exp' | number

export type ParamSpec = {
  id: ParamId
  label: string
  min: number
  max: number
  default: number
  unit: string
  taper: Taper
  /** Readout precision in decimal places. */
  digits: number
  /** Arrow-key / wheel step, in value units. */
  step: number
  /**
   * Value the dial fills *out from*. Defaults to `min`. Set it for controls
   * with a real neutral point, so "no change" reads as an empty dial rather
   * than a half-full one.
   */
  origin?: number
}

const spec = (
  id: ParamId,
  label: string,
  min: number,
  max: number,
  def: number,
  unit: string,
  taper: Taper,
  digits: number,
  step: number,
  origin?: number,
): ParamSpec => ({
  id,
  label,
  min,
  max,
  default: def,
  unit,
  taper,
  digits,
  step,
  origin,
})

export const PARAMS: Record<ParamId, ParamSpec> = {
  // Musical delay times live in the low hundreds of ms; an exponential taper
  // keeps eighth-note territory off the very bottom of the travel.
  timeMsL: spec('timeMsL', 'Left', 1, 4000, 375, 'ms', 'exp', 0, 1),
  timeMsR: spec('timeMsR', 'Right', 1, 4000, 563, 'ms', 'exp', 0, 1),
  feedback: spec('feedback', 'Repeats', 0, 98, 34, '%', 'lin', 0, 1),
  crossFeedback: spec('crossFeedback', 'Bounce', 0, 100, 65, '%', 'lin', 0, 1),
  lowCutHz: spec('lowCutHz', 'Low Cut', 20, 2000, 180, 'Hz', 'exp', 0, 1),
  highCutHz: spec('highCutHz', 'High Cut', 1000, 20000, 9000, 'Hz', 'exp', 0, 10),
  saturation: spec('saturation', 'Drive', 0, 100, 8, '%', 'lin', 0, 1),
  mix: spec('mix', 'Mix', 0, 100, 20, '%', 'lin', 0, 1),
  outputDb: spec('outputDb', 'Output', -24, 12, 0, 'dB', 'lin', 1, 0.5, 0),
}

export const MODES = ['stereo', 'pingpong', 'mono'] as const
export type Mode = (typeof MODES)[number]

export const MODE_LABELS: Record<Mode, string> = {
  stereo: 'Stereo',
  pingpong: 'Ping-Pong',
  mono: 'Mono',
}

/** One plain sentence under the mode chips — what the picture will do. */
export const MODE_HINTS: Record<Mode, string> = {
  stereo: 'Left and right keep their own echo spacing.',
  pingpong: 'Echoes bounce left ↔ right on each repeat.',
  mono: 'One shared delay — both sides use the same time.',
}

/** Wire value for `mode`; the index in [`MODES`] is the contract. */
export function modeToWire(mode: Mode): number {
  const index = MODES.indexOf(mode)
  return index < 0 ? MODES.indexOf('pingpong') : index
}

export function modeFromWire(value: number): Mode {
  return MODES[Math.round(value)] ?? 'pingpong'
}

export function clamp(value: number, min: number, max: number): number {
  return value < min ? min : value > max ? max : value
}

/** Value -> 0..1 control travel. */
export function toNorm(spec: ParamSpec, value: number): number {
  const v = clamp(value, spec.min, spec.max)
  if (spec.taper === 'exp') {
    const lo = Math.log(Math.max(spec.min, 1e-6))
    return (Math.log(Math.max(v, 1e-6)) - lo) / (Math.log(spec.max) - lo)
  }
  const t = (v - spec.min) / (spec.max - spec.min)
  return spec.taper === 'lin' ? t : Math.pow(t, 1 / spec.taper)
}

/**
 * 0..1 control travel -> value.
 *
 * The endpoints return `min`/`max` exactly rather than evaluating the taper:
 * `exp(log(4000))` lands a few ulps *short* of 4000, so a knob at full travel
 * would report 3999.9999999999995 ms and never quite reach its own maximum.
 * The interior result is clamped too, so no taper can overshoot the range.
 */
export function fromNorm(spec: ParamSpec, norm: number): number {
  const t = clamp(norm, 0, 1)
  if (t <= 0) return spec.min
  if (t >= 1) return spec.max
  if (spec.taper === 'exp') {
    const lo = Math.log(Math.max(spec.min, 1e-6))
    return clamp(
      Math.exp(lo + t * (Math.log(spec.max) - lo)),
      spec.min,
      spec.max,
    )
  }
  const shaped = spec.taper === 'lin' ? t : Math.pow(t, spec.taper)
  return clamp(spec.min + shaped * (spec.max - spec.min), spec.min, spec.max)
}

/** Readout text. Frequencies and delay times switch to a larger unit where the
 * four-digit form stops scanning quickly. */
export function format(spec: ParamSpec, value: number): string {
  const v = clamp(value, spec.min, spec.max)
  if (spec.unit === 'Hz' && v >= 1000) {
    return `${(v / 1000).toFixed(v >= 10000 ? 1 : 2)}k`
  }
  if (spec.unit === 'ms' && v >= 1000) {
    return `${(v / 1000).toFixed(2)}`
  }
  if (spec.unit === 'dB' && v > 0) {
    return `+${v.toFixed(spec.digits)}`
  }
  return v.toFixed(spec.digits)
}

/** Unit shown next to the readout — delay times flip to seconds past 1000 ms. */
export function unitFor(spec: ParamSpec, value: number): string {
  if (spec.unit === 'ms' && clamp(value, spec.min, spec.max) >= 1000) return 's'
  return spec.unit
}
