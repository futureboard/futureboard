import { DYN_KNEE_DB, type Band } from './bands'

export interface DynStep {
  /** Smoothed engagement, 0..1. */
  env: number
  /** Gain offset to apply to the band, in dB. */
  delta: number
}

/**
 * One control-rate step of a band's dynamics.
 *
 * `over` is how far the measured band level sits past the threshold in the
 * engaging direction; it maps across a `DYN_KNEE_DB` soft knee to 0..1, which is
 * then smoothed by a one-pole using attack going up and release coming down.
 * The band's gain moves by `env * dynRange`.
 */
export function dynamicStep(
  band: Pick<Band, 'threshold' | 'dynMode' | 'dynRange' | 'attack' | 'release'>,
  levelDb: number,
  env: number,
  dt: number,
): DynStep {
  const over = band.dynMode === 'above' ? levelDb - band.threshold : band.threshold - levelDb
  const target = Math.min(Math.max(over / DYN_KNEE_DB, 0), 1)

  const tauMs = target > env ? band.attack : band.release
  const tau = Math.max(tauMs / 1000, 0.001)
  const next = env + (target - env) * (1 - Math.exp(-dt / tau))

  return { env: next, delta: next * band.dynRange }
}

/** RMS of a time-domain block, in dBFS. */
export function rmsDb(buf: Float32Array): number {
  let sum = 0
  for (let i = 0; i < buf.length; i++) sum += buf[i] * buf[i]
  return 20 * Math.log10(Math.max(Math.sqrt(sum / buf.length), 1e-6))
}
