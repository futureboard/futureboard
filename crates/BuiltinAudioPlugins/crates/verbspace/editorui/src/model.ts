/**
 * Analytic model of the tank the DSP builds, used to draw the decay display.
 *
 * Every constant here mirrors `src/lib.rs`; the display is a *derived picture
 * of the current parameters*, not measured audio. Nothing in this file feeds
 * the DSP — if a number drifts from Rust the display is wrong, which is why
 * `model.test.ts` pins the constants against the Rust source.
 */

import type { Mode } from './params'
import type { VerbParams } from './bridge'

/** `verbspace::BASE_LINE_MS`. */
export const BASE_LINE_MS = [26.1, 31.7, 38.9, 44.3, 52.7, 59.9, 67.1, 73.7]

/** `verbspace::ReverbMode::line_scale`. */
export const MODE_LINE_SCALE: Record<Mode, number> = {
  room: 0.62,
  chamber: 0.78,
  hall: 1.0,
  plate: 0.42,
  ambience: 0.3,
}

/** `verbspace::ReverbMode::diffusion_bias`. */
export const MODE_DIFFUSION_BIAS: Record<Mode, number> = {
  room: 0.1,
  chamber: 0.18,
  hall: 0.22,
  plate: 0.45,
  ambience: 0.06,
}

/** Damping's one-pole coefficient reaches `damping/100 * DAMP_SCALE`. */
const DAMP_SCALE = 0.88

/** Tank line lengths in milliseconds at the current mode and size. */
export function lineDelaysMs(mode: Mode, size: number): number[] {
  const scale = MODE_LINE_SCALE[mode] * (0.35 + (size / 100) * 0.85)
  return BASE_LINE_MS.map((ms) => ms * scale)
}

/** Resolved allpass coefficient, matching `apply_params_scalars`. */
export function diffusionGain(mode: Mode, diffusion: number): number {
  const bias = MODE_DIFFUSION_BIAS[mode]
  return bias + (diffusion / 100) * (0.78 - bias)
}

export type DecayModel = {
  /** RT60 in seconds for the band below the 400 Hz split. */
  lowSec: number
  /** RT60 in seconds for the band above it, before damping. */
  midSec: number
  /** RT60 in seconds at Nyquist, after the damping low-pass. */
  highSec: number
  /** First arrival of each tank line, in milliseconds after the pre-delay. */
  lineDelaysMs: number[]
  predelayMs: number
  /** Longest RT60 present, for choosing a time axis. */
  longestSec: number
}

export function decayModel(params: VerbParams): DecayModel {
  const delays = lineDelaysMs(params.mode, params.size)
  const midSec = params.freeze ? Infinity : params.decaySec
  const lowSec = params.freeze ? Infinity : params.decaySec * params.bassMult

  // A damping coefficient `a` costs the top octave `(1 - a) / (1 + a)` of
  // amplitude per pass, on top of the RT60 loss. Solving the two together for
  // an equivalent RT60 is what lets the display show damping as a shorter
  // high-frequency tail rather than as an unexplained tilt.
  const a = (params.damping / 100) * DAMP_SCALE
  const hfPerPass = (1 - a) / (1 + a)
  let highSec = midSec
  if (!params.freeze && a > 0) {
    const meanDelaySec =
      delays.reduce((sum, ms) => sum + ms, 0) / delays.length / 1000
    const rt60Loss = (-3 * meanDelaySec) / Math.max(params.decaySec, 0.05)
    const dampLoss = Math.log10(Math.max(hfPerPass, 1e-6))
    highSec = (params.decaySec * rt60Loss) / (rt60Loss + dampLoss)
  }

  return {
    lowSec,
    midSec,
    highSec: Math.max(highSec, 0.01),
    lineDelaysMs: delays,
    predelayMs: params.predelayMs,
    longestSec: Math.max(lowSec, midSec, highSec),
  }
}

/** Amplitude in dB at `t` seconds for a band whose RT60 is `rt60`. */
export function levelDbAt(t: number, predelaySec: number, rt60: number): number {
  if (t < predelaySec) return -Infinity
  if (!Number.isFinite(rt60)) return 0
  return (-60 * (t - predelaySec)) / rt60
}
