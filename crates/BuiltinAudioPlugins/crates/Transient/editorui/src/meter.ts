/**
 * Display mapping for 67Clipper's scrolling waveform stage.
 *
 * Numbers come from the host's `futureboard.meters` frames. This module only
 * places those readings on the face and keeps a short rolling history so the
 * scrolling stage can draw them. Nothing here invents levels.
 */

export function clamp(value: number, min: number, max: number): number {
  return value < min ? min : value > max ? max : value
}

export function linearToDb(linear: number): number {
  return 20 * Math.log10(Math.max(linear, 1e-6))
}

/** Bottom of the scrolling stage / vertical level meters, in dBFS. */
export const STAGE_FLOOR_DB = -48

/** Top of the level scale (0 dBFS). */
export const STAGE_CEIL_DB = 0

/** Full-scale gain reduction for meters and the red GR overlay. */
export const GR_FULL_SCALE_DB = 24

/** How many history columns the scrolling stage keeps. */
export const HISTORY_LENGTH = 240

export type HistorySample = {
  /** Input peak, dBFS. */
  inDb: number
  /** Smoothed input RMS, dBFS. */
  rmsDb: number
  /** Output peak, dBFS. */
  outDb: number
  /** Positive gain reduction / clip depth in dB. */
  grDb: number
}

/**
 * Map a level in dBFS onto the stage: 0 at the top (0 dB), 1 at the floor.
 * Used for both the scrolling fill and the vertical bar meters.
 */
export function levelNorm(db: number): number {
  return clamp((STAGE_CEIL_DB - db) / (STAGE_CEIL_DB - STAGE_FLOOR_DB), 0, 1)
}

/**
 * Gain reduction / clip depth as a fraction of the stage height growing
 * downward from the top (0 dB line).
 */
export function grNorm(grDb: number): number {
  return clamp(grDb / GR_FULL_SCALE_DB, 0, 1)
}

/** Major dB marks printed on the stage scale. */
export const STAGE_TICKS_DB = [0, -3, -6, -9, -12, -18, -24, -36, -48] as const

/**
 * One-pole ballistics for meter fill. Fast enough to feel alive at ~30 Hz
 * host telemetry without inventing peaks.
 */
export function advance(
  current: number,
  target: number,
  dt: number,
  tau = 0.04,
): number {
  if (!Number.isFinite(target)) return current
  if (dt <= 0) return current
  const alpha = 1 - Math.exp(-dt / Math.max(tau, 1e-4))
  return current + (target - current) * alpha
}

/** Push one measured frame into a fixed-length ring (drops the oldest). */
export function pushHistory(
  history: readonly HistorySample[],
  sample: HistorySample,
  length = HISTORY_LENGTH,
): HistorySample[] {
  if (history.length >= length) {
    return [...history.slice(history.length - length + 1), sample]
  }
  return [...history, sample]
}

/** Empty history column used until the host starts sending meters. */
export function silentSample(): HistorySample {
  return {
    inDb: STAGE_FLOOR_DB,
    rmsDb: STAGE_FLOOR_DB,
    outDb: STAGE_FLOOR_DB,
    grDb: 0,
  }
}

/** Whether a GR/clip spike is worth tagging on the stage. */
export function shouldTagPeak(grDb: number, previousGrDb: number): boolean {
  return grDb >= 0.3 && grDb > previousGrDb + 0.15
}
