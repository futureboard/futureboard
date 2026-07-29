/**
 * VU scale, LED ladder mapping, and ballistics.
 *
 * Levels come from host `futureboard.meters` — this module only maps readings
 * onto the face and moves needles/LEDs with hardware-correct time constants.
 * Nothing here invents a level.
 */

export type MeterMode = 'reduction' | 'output'

/**
 * VU ballistics: a step reaches 99 % in 300 ms → 4.6 τ, so τ = 300/4.6 ms.
 * Real elapsed `dt` keeps the feel correct at 60 Hz or when throttled.
 */
export const VU_TAU_SECONDS = 0.3 / 4.6

/** LED peak rise is near-instant; fall is slower so the ladder reads. */
export const LED_ATTACK_TAU = 0.008
export const LED_RELEASE_TAU = 0.09

/** Peak-hold dwell then linear fall (hardware LED meters). */
export const PEAK_HOLD_SECONDS = 1.15
export const PEAK_HOLD_FALL_DB_PER_SEC = 14

/** Needle sweep either side of vertical. */
export const HALF_SWEEP_DEG = 40

/** Far-left of the GAIN REDUCTION scale. */
export const REDUCTION_FULL_SCALE_DB = 24

/** OUTPUT scale ends, dB relative to 0 VU. */
export const OUTPUT_MIN_DB = -20
export const OUTPUT_MAX_DB = 3

/** LED ladder floor / ceiling (dBFS). */
export const LADDER_FLOOR_DB = -60
export const LADDER_CEIL_DB = 0
export const LADDER_SEGMENTS = 24

/** Quiet-end expansion on the GR face (matches FA enamel spacing). */
const REDUCTION_CURVE = 0.6

export function clamp(value: number, min: number, max: number) {
  return value < min ? min : value > max ? max : value
}

export function linearToDb(linear: number) {
  return 20 * Math.log10(Math.max(linear, 1e-6))
}

/**
 * One-pole step. Optional separate attack/release τ for LED peak edges.
 */
export function advance(
  current: number,
  target: number,
  dt: number,
  tau = VU_TAU_SECONDS,
  releaseTau = tau,
): number {
  if (!Number.isFinite(target) || dt <= 0) return current
  const useTau = target > current ? tau : releaseTau
  const alpha = 1 - Math.exp(-dt / Math.max(useTau, 1e-4))
  return current + (target - current) * alpha
}

/** 0 left … 1 right. Reduction rests right and swings left as GR grows. */
export function normFor(mode: MeterMode, value: number) {
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

/** Degrees from vertical; positive = right. */
export function angleFor(norm: number) {
  return (clamp(norm, 0, 1) * 2 - 1) * HALF_SWEEP_DEG
}

/** Linear amplitude → ladder fill 0…1 (bottom quiet → top hot). */
export function ladderNormFromLinear(linear: number) {
  const db = linearToDb(linear)
  return clamp((db - LADDER_FLOOR_DB) / (LADDER_CEIL_DB - LADDER_FLOOR_DB), 0, 1)
}

export function formatLadderDb(linear: number) {
  if (linear < 1e-6) return '−∞'
  const db = linearToDb(linear)
  if (db <= LADDER_FLOOR_DB + 0.5) return '−∞'
  return db.toFixed(0)
}

export type Tick = {
  label: string
  norm: number
  major: boolean
  hot: boolean
}

const REDUCTION_TICKS = [0, 1.5, 3, 4.5, 6, 9, 12, 18, 24]
const OUTPUT_TICKS = [-20, -10, -7, -5, -3, -1, 0, 1, 3]
const REDUCTION_LABELLED = new Set([0, 3, 6, 12, 24])
const OUTPUT_LABELLED = new Set([-20, -10, -7, -3, 0, 3])

export function ticksFor(mode: MeterMode): Tick[] {
  if (mode === 'reduction') {
    return REDUCTION_TICKS.map((db) => ({
      label: REDUCTION_LABELLED.has(db) ? String(db) : '',
      norm: normFor('reduction', db),
      major: REDUCTION_LABELLED.has(db),
      // Deep GR is a setting, not a fault — red reserved for output over 0 VU.
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

/** Mutable peak-hold cell — updated in the meter RAF, never allocated per frame. */
export type PeakHold = {
  db: number
  age: number
}

export function createPeakHold(): PeakHold {
  return { db: LADDER_FLOOR_DB, age: PEAK_HOLD_SECONDS }
}

export function pushPeakHold(hold: PeakHold, peakDb: number, dt: number) {
  if (peakDb >= hold.db - 0.05) {
    hold.db = peakDb
    hold.age = 0
    return
  }
  hold.age += dt
  if (hold.age < PEAK_HOLD_SECONDS) return
  hold.db = Math.max(
    LADDER_FLOOR_DB,
    hold.db - PEAK_HOLD_FALL_DB_PER_SEC * dt,
  )
}

export function holdNorm(hold: PeakHold) {
  return clamp(
    (hold.db - LADDER_FLOOR_DB) / (LADDER_CEIL_DB - LADDER_FLOOR_DB),
    0,
    1,
  )
}
