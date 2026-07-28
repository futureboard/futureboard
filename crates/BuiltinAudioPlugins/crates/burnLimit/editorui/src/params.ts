/**
 * Editor-side mirror of BurnLimit's parameter contract.
 */

export type ParamId =
  | 'gainDb'
  | 'ceilingDb'
  | 'releaseMs'
  | 'lookaheadMs'
  | 'mix'

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
  gainDb: spec('gainDb', 'Gain', -12, 24, 0, 'dB', 'lin', 1, 0.1, 0),
  ceilingDb: spec('ceilingDb', 'Ceiling', -6, 0, -0.3, 'dB', 'lin', 1, 0.1, 0),
  releaseMs: spec('releaseMs', 'Release', 20, 2000, 200, 'ms', 'exp', 0, 1),
  lookaheadMs: spec('lookaheadMs', 'Lookahead', 0, 10, 2, 'ms', 'lin', 1, 0.1),
  mix: spec('mix', 'Mix', 0, 100, 100, '%', 'lin', 0, 1),
}

export const STYLES = ['clean', 'punch', 'modern', 'clip'] as const
export type Style = (typeof STYLES)[number]

export const STYLE_LABELS: Record<Style, string> = {
  clean: 'Clean',
  punch: 'Punch',
  modern: 'Modern',
  clip: 'Clip',
}

export function styleToWire(style: Style): number {
  const index = STYLES.indexOf(style)
  return index < 0 ? STYLES.indexOf('modern') : index
}

export function styleFromWire(value: number): Style {
  return STYLES[Math.round(value)] ?? 'modern'
}

export function clamp(value: number, min: number, max: number): number {
  return value < min ? min : value > max ? max : value
}

export function toNorm(spec: ParamSpec, value: number): number {
  const v = clamp(value, spec.min, spec.max)
  if (spec.taper === 'exp') {
    const lo = Math.log(Math.max(spec.min, 1e-6))
    return (Math.log(Math.max(v, 1e-6)) - lo) / (Math.log(spec.max) - lo)
  }
  const t = (v - spec.min) / (spec.max - spec.min)
  return spec.taper === 'lin' ? t : Math.pow(t, 1 / spec.taper)
}

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
