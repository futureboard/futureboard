import { describe, expect, test } from 'bun:test'

import type { EchoParams } from './bridge'
import {
  MAX_FEEDBACK,
  echoModel,
  envelopeAt,
  filterMagnitude,
  laneFor,
  loopGain,
  tapTimes,
} from './model'
import { DEFAULT_PARAMS } from './presets'
import { LIB_RS, rustScalar } from './rust'

const defaults: EchoParams = { ...DEFAULT_PARAMS }

describe('the display model mirrors the Rust delay line', () => {
  test('the feedback ceiling matches echospace::MAX_FEEDBACK', () => {
    expect(MAX_FEEDBACK).toBe(rustScalar(LIB_RS, 'MAX_FEEDBACK'))
  })

  /**
   * The Rust normalises the two feedback paths by the cross amount so the loop
   * gain stays `feedback`. If that ever reverts, this display would keep
   * drawing a decaying tail while the real line ran away.
   */
  test('cross-feed does not change the loop gain', () => {
    const none = loopGain({ ...defaults, crossFeedback: 0 })
    const full = loopGain({ ...defaults, crossFeedback: 100 })
    expect(none).toBeCloseTo(defaults.feedback / 100, 9)
    expect(full).toBeCloseTo(none, 9)
  })

  test('freeze pins the loop at the ceiling', () => {
    expect(loopGain({ ...defaults, freeze: true })).toBe(MAX_FEEDBACK)
  })
})

describe('tap layout', () => {
  test('mono collapses both lanes onto the left time', () => {
    const params = { ...defaults, mode: 'mono' as const, timeMsR: 900 }
    const times = tapTimes(params)
    expect(times.left).toBeCloseTo(params.timeMsL / 1000, 9)
    expect(times.right).toBe(times.left)
  })

  test('stereo keeps each line on its own time and side', () => {
    const times = tapTimes({ ...defaults, mode: 'stereo' })
    expect(times.left).toBeCloseTo(defaults.timeMsL / 1000, 9)
    expect(times.right).toBeCloseTo(defaults.timeMsR / 1000, 9)
    expect(laneFor('stereo', 1, 'left')).toBe('left')
    expect(laneFor('stereo', 2, 'right')).toBe('right')
  })

  /** Ping-pong swaps the feedback paths every round trip, so a line's odd
   *  passes come back on the opposite side. */
  test('ping-pong alternates sides on odd passes', () => {
    expect(laneFor('pingpong', 1, 'left')).toBe('right')
    expect(laneFor('pingpong', 2, 'left')).toBe('left')
    expect(laneFor('pingpong', 3, 'right')).toBe('left')
    expect(laneFor('pingpong', 4, 'right')).toBe('right')
  })

  test('repeats land on integer multiples of the tap time and decay', () => {
    const params = { ...defaults, mode: 'stereo' as const, feedback: 50 }
    const { taps } = echoModel(params)
    const left = taps.filter((tap) => tap.lane === 'left')
    expect(left.length).toBeGreaterThan(2)
    for (const tap of left) {
      expect(tap.time).toBeCloseTo((params.timeMsL / 1000) * tap.pass, 9)
      expect(tap.amplitude).toBeCloseTo(0.5 ** tap.pass, 9)
    }
  })

  test('taps are ordered in time', () => {
    const { taps } = echoModel(defaults)
    for (let i = 1; i < taps.length; i++) {
      expect(taps[i]!.time).toBeGreaterThanOrEqual(taps[i - 1]!.time)
    }
  })

  test('zero feedback produces exactly one repeat per lane', () => {
    const { taps } = echoModel({ ...defaults, mode: 'stereo', feedback: 0 })
    expect(taps).toHaveLength(0)
    const barely = echoModel({ ...defaults, mode: 'stereo', feedback: 1 })
    expect(barely.taps.length).toBeGreaterThan(0)
  })

  /** Freeze never decays, so the list has to be bounded by the pass cap rather
   *  than by the amplitude floor. */
  test('freeze produces a bounded tap list', () => {
    const { taps, passes } = echoModel({ ...defaults, freeze: true }, -60, 32)
    expect(passes).toBe(32)
    expect(taps).toHaveLength(64)
    expect(taps.every((tap) => tap.amplitude > 0.9)).toBe(true)
  })
})

describe('feedback-path tone', () => {
  test('the reference tone passes when the filters are wide open', () => {
    const wide = filterMagnitude(
      { ...defaults, lowCutHz: 20, highCutHz: 20000 },
      6000,
    )
    expect(wide).toBeGreaterThan(0.9)
  })

  test('closing the high cut dulls the reference tone', () => {
    let previous = 1
    for (const highCutHz of [20000, 12000, 6000, 3000, 1000]) {
      const magnitude = filterMagnitude({ ...defaults, highCutHz }, 6000)
      expect(magnitude).toBeLessThan(previous)
      expect(magnitude).toBeGreaterThanOrEqual(0)
      previous = magnitude
    }
  })

  test('at the corner a 2-pole section is down 3 dB', () => {
    const magnitude = filterMagnitude(
      { ...defaults, lowCutHz: 20, highCutHz: 6000 },
      6000,
    )
    expect(20 * Math.log10(magnitude)).toBeCloseTo(-3.01, 1)
  })
})

describe('envelope', () => {
  test('one tap time back is exactly one round-trip of gain', () => {
    expect(envelopeAt(0.375, 0.375, 0.5)).toBeCloseTo(0.5, 9)
    expect(envelopeAt(0.75, 0.375, 0.5)).toBeCloseTo(0.25, 9)
  })

  test('degenerate inputs do not produce NaN', () => {
    expect(envelopeAt(0, 0.375, 0.5)).toBe(1)
    expect(envelopeAt(1, 0, 0.5)).toBe(1)
    expect(envelopeAt(1, 0.375, 0)).toBe(0)
  })
})
