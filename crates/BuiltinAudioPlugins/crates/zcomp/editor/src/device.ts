/**
 * Faceplate geometry and the display-side mirror of the Rust gain computer.
 *
 * The panel is laid out once at a fixed design size and scaled to whatever
 * client rectangle the native host gives the browser. Every coordinate below is
 * in *design pixels*: one layout owner, no percentage heights, no circular
 * sizing, and identical proportions at any window size or DPI.
 */

import type { CompModel, ZcompParams } from './bridge'
import { clamp } from './meter'

/** Design canvas. 900 x 620 is the editor window minimum, minus native chrome. */
export const DEVICE_WIDTH = 1020
export const DEVICE_HEIGHT = 600

/** Beyond this the faceplate stops growing and simply centres. */
export const MAX_SCALE = 1.7

/** Knob pointer travel: 300 degrees, resting at 7 o'clock like real hardware. */
export const KNOB_START_DEG = -150
export const KNOB_SWEEP_DEG = 300

/** Vertical drag distance (px) for the full range of a knob. */
export const KNOB_TRAVEL_PX = 260

/**
 * Static gain-computer constants for one model.
 *
 * Mirrors `zcomp::dsp::model_coeffs` for the four values that shape the
 * transfer curve. Rust stays authoritative for what the audio actually does —
 * this exists so the curve window can draw the cell the user is hearing
 * instead of a generic textbook knee.
 */
export type CircuitCurve = {
  thresholdDb: number
  ratio: number
  kneeDb: number
  /** Overshoot (dB) at which an optical cell is halfway to its ratio. */
  overEasyDb: number
}

export function circuitCurve(params: ZcompParams): CircuitCurve {
  const color = clamp(params.color, 0, 100) / 100
  const ratio = Math.max(params.ratio, 1)
  const knee = Math.max(params.kneeDb, 0)

  switch (params.model) {
    case 'comp2500':
      return {
        thresholdDb: params.thresholdDb,
        ratio,
        kneeDb: Math.min(knee + 3 + color * 3, 24),
        overEasyDb: 0,
      }
    case 'distressor':
      return {
        thresholdDb: params.thresholdDb - color * 1.5,
        ratio: Math.min(ratio * (1 + color * 0.5), 40),
        kneeDb: Math.max(knee * (1 - color * 0.6), 0.3),
        overEasyDb: 0,
      }
    case 'avalon':
      return {
        thresholdDb: params.thresholdDb,
        ratio: clamp(2 + (ratio - 1) * 0.55, 1.5, 8),
        kneeDb: Math.min(knee + 6 + color * 4, 24),
        overEasyDb: 8,
      }
    case 'ssl':
    default:
      return {
        thresholdDb: params.thresholdDb,
        ratio: clamp(ratio, 1.5, 10),
        kneeDb: clamp(knee + 3, 2, 18),
        overEasyDb: 0,
      }
  }
}

/** Reduction the cell would apply to a steady level. Mirrors `target_gr_db`. */
export function targetGrDb(curve: CircuitCurve, levelDb: number) {
  const over = levelDb - curve.thresholdDb
  const halfKnee = curve.kneeDb * 0.5
  if (over <= -halfKnee) return 0

  let curved: number
  if (over >= halfKnee || curve.kneeDb <= 1e-4) {
    curved = Math.max(over, 0)
  } else {
    const t = over + halfKnee
    curved = (t * t) / (2 * curve.kneeDb)
  }

  const ratio =
    curve.overEasyDb > 0
      ? 1 + (curve.ratio - 1) * (curved / (curved + curve.overEasyDb))
      : curve.ratio

  return curved * (1 - 1 / ratio)
}

/** Engraved plate text for each circuit. */
export const CIRCUIT_INFO: Record<
  CompModel,
  { name: string; topology: string }
> = {
  comp2500: { name: '2500', topology: 'VCA · feed-forward · thrust sidechain' },
  distressor: { name: 'Distress', topology: 'FET · feedback loop · British grit' },
  avalon: { name: 'Avalon', topology: 'Opto · Class-A · over-easy ratio' },
  ssl: { name: 'SSL', topology: 'Bus VCA · dual time-constant release' },
}
