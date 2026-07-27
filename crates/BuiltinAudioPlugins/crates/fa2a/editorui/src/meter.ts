/**
 * VU meter scale and ballistics.
 *
 * The numbers the needle shows come from the plugin host's telemetry frame
 * (`futureboard.meters`) — this module only decides where on the face a given
 * reading sits and how fast the needle is allowed to get there. Nothing here
 * invents a level.
 */

/** What the meter is pointed at, matching the hardware's meter switch. */
export type MeterMode = 'reduction' | 'output'

/**
 * VU ballistics: a step input reaches 99 % of its final deflection in 300 ms.
 * For a one-pole that is 4.6 time constants, so tau is 300/4.6 ms. Using the
 * real figure means the needle lags exactly as much as the meter it is drawn
 * to look like.
 */
export const VU_TAU_SECONDS = 0.3 / 4.6

/** Sweep of the needle either side of vertical, in degrees. */
export const HALF_SWEEP_DEG = 40

/** Reduction, in dB, at the far left of the GAIN REDUCTION scale. */
export const REDUCTION_FULL_SCALE_DB = 20

/** Ends of the OUTPUT scale, in dB relative to the meter's 0 VU. */
export const OUTPUT_MIN_DB = -20
export const OUTPUT_MAX_DB = 3

export function clamp(value: number, min: number, max: number): number {
  return value < min ? min : value > max ? max : value
}

export function linearToDb(linear: number): number {
  return 20 * Math.log10(Math.max(linear, 1e-6))
}

/**
 * How much the reduction scale is expanded at the quiet end.
 *
 * Linear in dB puts 0..6 dB — where a leveller actually works — in the last
 * 30 % of the face, leaving most of the scale for reduction depths nobody
 * dials in. This exponent gives the first 6 dB about half the travel, which
 * is also roughly how the hardware's face is spaced.
 */
const REDUCTION_CURVE = 0.6

/**
 * Position on the face, 0 at the left stop and 1 at the right.
 *
 * GAIN REDUCTION runs backwards, as it does on the hardware: the needle rests
 * at 0 on the right and swings left as the cell takes more off.
 */
export function normFor(mode: MeterMode, value: number): number {
  if (mode === 'reduction') {
    const t = clamp(value / REDUCTION_FULL_SCALE_DB, 0, 1)
    return clamp(1 - Math.pow(t, REDUCTION_CURVE), 0, 1)
  }
  return clamp(
    (value - OUTPUT_MIN_DB) / (OUTPUT_MAX_DB - OUTPUT_MIN_DB),
    0,
    1,
  )
}

/** Needle angle in degrees from vertical, positive to the right. */
export function angleFor(norm: number): number {
  return (clamp(norm, 0, 1) * 2 - 1) * HALF_SWEEP_DEG
}

export type Tick = {
  /** Printed on the face; empty for a minor tick. */
  label: string
  norm: number
  major: boolean
  /** Past 0 VU / into heavy reduction — drawn in the face's red. */
  hot: boolean
}

const REDUCTION_TICKS = [0, 1, 2, 3, 4, 5, 6, 8, 10, 15, 20]
const OUTPUT_TICKS = [-20, -10, -7, -5, -3, -2, -1, 0, 1, 2, 3]

/** Which readings get a printed number, to keep the face from crowding. */
const REDUCTION_LABELLED = new Set([0, 2, 4, 6, 10, 20])
const OUTPUT_LABELLED = new Set([-20, -10, -7, -5, -3, 0, 3])

export function ticksFor(mode: MeterMode): Tick[] {
  if (mode === 'reduction') {
    return REDUCTION_TICKS.map((db) => ({
      label: REDUCTION_LABELLED.has(db) ? String(db) : '',
      norm: normFor('reduction', db),
      major: REDUCTION_LABELLED.has(db),
      // No red on this scale: deep reduction is a setting, not a fault. Red
      // is reserved for the output scale above 0 VU.
      hot: false,
    }))
  }
  return OUTPUT_TICKS.map((db) => ({
    label: OUTPUT_LABELLED.has(db)
      ? db > 0
        ? `+${db}`
        : String(db)
      : '',
    norm: normFor('output', db),
    major: OUTPUT_LABELLED.has(db),
    hot: db >= 0,
  }))
}

/**
 * One step of needle movement.
 *
 * `dt` is the real elapsed time, so the ballistics stay correct whether the
 * page is running at 60 Hz or has been throttled in a background window.
 */
export function advance(
  current: number,
  target: number,
  dt: number,
  tau = VU_TAU_SECONDS,
): number {
  if (!Number.isFinite(target)) return current
  if (dt <= 0) return current
  const alpha = 1 - Math.exp(-dt / Math.max(tau, 1e-4))
  return current + (target - current) * alpha
}
