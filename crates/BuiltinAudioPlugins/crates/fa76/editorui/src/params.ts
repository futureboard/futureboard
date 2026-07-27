/**
 * Editor-side mirror of FA-76's parameter contract.
 *
 * Rust owns the authority: `src/ipc.rs` defines the ids and their ranges, and
 * clamps every incoming value again. This table exists so the UI can lay out a
 * control, choose a taper, and format a readout — never to decide what a value
 * means. `params.test.ts` pins the ids and ranges against the Rust source.
 */

export type ParamId =
  | 'inputDb'
  | 'outputDb'
  | 'attackUs'
  | 'releaseMs'
  | 'mix'
  | 'sidechainHpfHz'

type Taper = 'lin' | 'exp' | number

export type ParamSpec = {
  id: ParamId
  label: string
  min: number
  max: number
  default: number
  unit: string
  taper: Taper
  digits: number
  step: number
  /** Value the dial fills out from. Defaults to `min`. */
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
  inputDb: spec('inputDb', 'Input', -12, 36, 18, 'dB', 'lin', 1, 0.5, 0),
  outputDb: spec('outputDb', 'Output', -36, 12, -12, 'dB', 'lin', 1, 0.5, 0),
  attackUs: spec('attackUs', 'Attack', 20, 800, 20, 'µs', 'exp', 0, 1),
  releaseMs: spec('releaseMs', 'Release', 50, 1100, 100, 'ms', 'exp', 0, 1),
  mix: spec('mix', 'Mix', 0, 100, 100, '%', 'lin', 0, 1),
  sidechainHpfHz: spec(
    'sidechainHpfHz',
    'SC HPF',
    0,
    500,
    60,
    'Hz',
    'lin',
    0,
    1,
  ),
}

/** Ratio pushbuttons; index is the wire contract with `RatioButton::to_wire`. */
export const RATIOS = ['r4', 'r8', 'r12', 'r20', 'all'] as const
export type Ratio = (typeof RATIOS)[number]

/** Labels printed on the FET faceplate. */
export const RATIO_LABELS: Record<Ratio, string> = {
  r4: '4',
  r8: '8',
  r12: '12',
  r20: '20',
  all: 'All',
}

/** Wire value for `ratio`; the index in [`RATIOS`] is the contract. */
export function ratioToWire(ratio: Ratio): number {
  const index = RATIOS.indexOf(ratio)
  return index < 0 ? RATIOS.indexOf('r4') : index
}

export function ratioFromWire(value: number): Ratio {
  return RATIOS[Math.round(value)] ?? 'r4'
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
 * `exp(log(max))` lands a few ulps short, so a knob at full travel would never
 * quite reach its own maximum.
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

export function format(spec: ParamSpec, value: number): string {
  const v = clamp(value, spec.min, spec.max)
  if (spec.unit === 'Hz' && v >= 1000) {
    return `${(v / 1000).toFixed(2)}k`
  }
  if (spec.unit === 'µs' && v >= 1000) {
    return `${(v / 1000).toFixed(2)}`
  }
  if (spec.unit === 'dB' && v > 0) {
    return `+${v.toFixed(spec.digits)}`
  }
  return v.toFixed(spec.digits)
}
