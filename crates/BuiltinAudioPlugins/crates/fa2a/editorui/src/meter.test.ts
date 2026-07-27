import { describe, expect, test } from 'bun:test'

import {
  HALF_SWEEP_DEG,
  OUTPUT_MAX_DB,
  OUTPUT_MIN_DB,
  REDUCTION_FULL_SCALE_DB,
  VU_TAU_SECONDS,
  advance,
  angleFor,
  linearToDb,
  normFor,
  ticksFor,
} from './meter'

describe('the gain reduction scale', () => {
  /** The hardware's GR meter rests at 0 on the right and swings left. */
  test('runs backwards, resting at the right stop', () => {
    expect(normFor('reduction', 0)).toBe(1)
    expect(normFor('reduction', REDUCTION_FULL_SCALE_DB)).toBe(0)
    expect(angleFor(normFor('reduction', 0))).toBe(HALF_SWEEP_DEG)
  })

  test('is monotonic — more reduction is always further left', () => {
    let previous = Infinity
    for (let db = 0; db <= REDUCTION_FULL_SCALE_DB; db += 0.5) {
      const norm = normFor('reduction', db)
      expect(norm).toBeLessThan(previous)
      previous = norm
    }
  })

  /**
   * The whole point of the curve: a leveller works in the first few dB, so
   * that range has to own a usable share of the face rather than being
   * squeezed against the right stop.
   */
  test('gives the first 6 dB roughly half the travel', () => {
    const used = 1 - normFor('reduction', 6)
    expect(used).toBeGreaterThan(0.4)
    expect(used).toBeLessThan(0.6)
  })

  test('clamps rather than running off the face', () => {
    expect(normFor('reduction', -5)).toBe(1)
    expect(normFor('reduction', 500)).toBe(0)
  })

  /** Red belongs to the output scale above 0 VU; deep reduction is a setting. */
  test('has no red zone', () => {
    expect(ticksFor('reduction').some((tick) => tick.hot)).toBe(false)
  })
})

describe('the output scale', () => {
  test('spans its declared range left to right', () => {
    expect(normFor('output', OUTPUT_MIN_DB)).toBe(0)
    expect(normFor('output', OUTPUT_MAX_DB)).toBe(1)
    expect(normFor('output', -20)).toBeLessThan(normFor('output', 0))
  })

  test('marks 0 VU and above in red', () => {
    const ticks = ticksFor('output')
    for (const tick of ticks) {
      const label = Number(tick.label.replace('+', ''))
      if (tick.label && Number.isFinite(label)) {
        expect(tick.hot).toBe(label >= 0)
      }
    }
  })
})

describe('ticks', () => {
  test('every tick lands on the face, in order, with labels only on majors', () => {
    for (const mode of ['reduction', 'output'] as const) {
      const ticks = ticksFor(mode)
      expect(ticks.length).toBeGreaterThan(4)
      for (const tick of ticks) {
        expect(tick.norm).toBeGreaterThanOrEqual(0)
        expect(tick.norm).toBeLessThanOrEqual(1)
        if (tick.label) expect(tick.major).toBe(true)
      }
      const sorted = [...ticks].sort((a, b) => a.norm - b.norm)
      expect(new Set(sorted.map((t) => t.norm)).size).toBe(ticks.length)
    }
  })
})

describe('needle ballistics', () => {
  /** Step response after `duration`, taken in fixed `dt` slices. The step
   *  count is computed rather than accumulated so the comparison below
   *  measures the ballistic, not the loop's rounding. */
  const settle = (dt: number, duration: number, target = 1) => {
    let needle = 0
    const steps = Math.round(duration / dt)
    for (let i = 0; i < steps; i++) needle = advance(needle, target, dt)
    return needle
  }

  /** The VU standard: 99 % of a step in 300 ms. */
  test('reaches 99 percent of a step in 300 ms', () => {
    const needle = settle(1 / 1000, 0.3)
    expect(needle).toBeGreaterThan(0.985)
    expect(needle).toBeLessThan(0.995)
  })

  test('is frame-rate independent', () => {
    expect(settle(1 / 240, 0.2)).toBeCloseTo(settle(1 / 30, 0.2), 6)
    expect(settle(1 / 144, 0.5)).toBeCloseTo(settle(1 / 60, 0.5), 6)
  })

  test('never overshoots and holds still without time or a target', () => {
    expect(advance(0.5, 1, 0)).toBe(0.5)
    expect(advance(0.5, Number.NaN, 0.1)).toBe(0.5)
    let needle = 0
    for (let i = 0; i < 10_000; i++) needle = advance(needle, 1, 0.01)
    expect(needle).toBeLessThanOrEqual(1)
  })

  test('tau is the documented VU figure', () => {
    expect(VU_TAU_SECONDS).toBeCloseTo(0.3 / 4.6, 9)
  })
})

describe('level conversion', () => {
  test('unity is 0 dB and silence floors instead of returning -Infinity', () => {
    expect(linearToDb(1)).toBeCloseTo(0, 9)
    expect(linearToDb(0.5)).toBeCloseTo(-6.02, 2)
    expect(Number.isFinite(linearToDb(0))).toBe(true)
  })
})
