/**
 * Editor-side mirror of 67Clipper's parameter contract.
 *
 * Rust owns the authority: `src/ipc.rs` defines the ids and their ranges, and
 * clamps every incoming value again. This table exists so the UI can lay out
 * a control, choose a taper, and format a readout — never to decide what a
 * value means. `params.test.ts` pins the ids and ranges against the Rust
 * source.
 */

export type ParamId = 'thresholdDb' | 'shape' | 'ceilingDb' | 'mix'

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
  /** Value the dial/arc fills out from. Defaults to `min`. */
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
  thresholdDb: spec('thresholdDb', 'Threshold', -24, 0, -6, 'dB', 'lin', 1, 0.1, 0),
  shape: spec('shape', 'Shape', 0, 100, 50, '%', 'lin', 0, 1),
  ceilingDb: spec('ceilingDb', 'Ceiling', -6, 0, -0.3, 'dB', 'lin', 1, 0.1, 0),
  mix: spec('mix', 'Mix', 0, 100, 100, '%', 'lin', 0, 1),
}

/** Processing mode pushbuttons; index is the `Mode::to_wire` contract. */
export const MODES = ['clip', 'hybrid', 'limit'] as const
export type Mode = (typeof MODES)[number]

export const MODE_LABELS: Record<Mode, string> = {
  clip: 'Clip',
  hybrid: 'Hybrid',
  limit: 'Limit',
}

/** Wire value for `mode`; the index in [`MODES`] is the contract. */
export function modeToWire(mode: Mode): number {
  const index = MODES.indexOf(mode)
  return index < 0 ? MODES.indexOf('clip') : index
}

/** Mirrors `Mode::from_wire`: round and fall back to Clip. */
export function modeFromWire(value: number): Mode {
  const rounded = Math.round(value)
  if (rounded === 1) return 'hybrid'
  if (rounded === 2) return 'limit'
  return 'clip'
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
 * `exp(log(max))` lands a few ulps short, so a knob at full travel would
 * never quite reach its own maximum.
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
  if (spec.unit === 'dB' && v > 0) {
    return `+${v.toFixed(spec.digits)}`
  }
  return v.toFixed(spec.digits)
}
