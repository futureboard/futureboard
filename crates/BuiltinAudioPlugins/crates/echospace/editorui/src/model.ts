/**
 * Analytic model of the repeat pattern the DSP produces, used to draw the echo
 * display.
 *
 * Every constant here mirrors `src/lib.rs`; the display is a *derived picture
 * of the current parameters*, not measured audio. Nothing in this file feeds
 * the DSP — if a number drifts from Rust the display is wrong, which is why
 * `model.test.ts` pins the constants against the Rust source.
 */

import type { Mode } from './params'
import type { EchoParams } from './bridge'

/** `echospace::MAX_FEEDBACK`. */
export const MAX_FEEDBACK = 0.9995

/** Which channel a repeat lands in. */
export type Lane = 'left' | 'right'

export type Tap = {
  /** Arrival time in seconds after the dry signal. */
  time: number
  /** Linear amplitude relative to the dry signal. */
  amplitude: number
  /** Round-trip number; 1 is the first repeat. */
  pass: number
  lane: Lane
}

/**
 * Round-trip gain, matching `process_stereo`.
 *
 * The two feedback paths are normalised by the cross amount so the loop gain
 * stays `feedback` however much of it is routed across — mirroring the Rust,
 * where summing them at full strength reached 1.96 and the line ran away.
 */
export function loopGain(params: EchoParams): number {
  if (params.freeze) return MAX_FEEDBACK
  return Math.min(params.feedback / 100, MAX_FEEDBACK)
}

/**
 * Per-pass magnitude of the feedback path's filters at `hz`.
 *
 * The low and high cut are 2-pole Butterworth sections (`Q = 0.707`), so each
 * has a magnitude of `1 / sqrt(1 + (f/fc)^4)` in its stopband direction. The
 * display uses this to dull each successive repeat by the amount the real
 * filters dull it.
 */
export function filterMagnitude(params: EchoParams, hz: number): number {
  const lowRatio = hz / Math.max(params.lowCutHz, 1)
  const highPass = lowRatio ** 2 / Math.sqrt(1 + lowRatio ** 4)
  const lowPass = 1 / Math.sqrt(1 + (hz / Math.max(params.highCutHz, 1)) ** 4)
  return highPass * lowPass
}

/** Delay time per channel in seconds, after the mode's routing. */
export function tapTimes(params: EchoParams): { left: number; right: number } {
  const left = params.timeMsL / 1000
  const right = params.timeMsR / 1000
  // Mono collapses both lines onto the left time — the DSP sums the input to
  // mono and both rings read the same tap.
  if (params.mode === 'mono') return { left, right: left }
  return { left, right }
}

/**
 * Which lane pass `pass` lands in.
 *
 * Ping-pong swaps the two feedback paths every round trip, so odd passes come
 * back on the opposite side. Stereo and mono keep each line on its own side.
 */
export function laneFor(mode: Mode, pass: number, lane: Lane): Lane {
  if (mode !== 'pingpong') return lane
  const flipped: Lane = lane === 'left' ? 'right' : 'left'
  return pass % 2 === 1 ? flipped : lane
}

export type EchoModel = {
  taps: Tap[]
  /** Time of the last audible repeat, in seconds. */
  lastTime: number
  /** Round-trip gain in use. */
  gain: number
  /** Number of repeats before the tail falls under `floor`. */
  passes: number
}

/**
 * The repeat pattern down to `floorDb`.
 *
 * Capped at `maxPasses` so a near-unity feedback (or freeze, which never
 * decays) produces a bounded list instead of running until the float
 * underflows.
 */
export function echoModel(
  params: EchoParams,
  floorDb = -60,
  maxPasses = 64,
): EchoModel {
  const gain = loopGain(params)
  const times = tapTimes(params)
  const floor = 10 ** (floorDb / 20)
  const taps: Tap[] = []
  let lastTime = 0

  for (const lane of ['left', 'right'] as const) {
    const step = times[lane]
    for (let pass = 1; pass <= maxPasses; pass++) {
      const amplitude = gain ** pass
      if (amplitude < floor) break
      const time = step * pass
      taps.push({ time, amplitude, pass, lane: laneFor(params.mode, pass, lane) })
      lastTime = Math.max(lastTime, time)
    }
  }

  taps.sort((a, b) => a.time - b.time)
  const passes = taps.reduce((most, tap) => Math.max(most, tap.pass), 0)
  return { taps, lastTime, gain, passes }
}

/** Amplitude of the feedback envelope at `t` seconds, for one lane's spacing. */
export function envelopeAt(t: number, step: number, gain: number): number {
  if (t <= 0 || step <= 0) return 1
  if (gain <= 0) return 0
  return gain ** (t / step)
}
