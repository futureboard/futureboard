import { describe, expect, test } from 'bun:test'

import {
  GR_FULL_SCALE_DB,
  HISTORY_LENGTH,
  STAGE_CEIL_DB,
  STAGE_FLOOR_DB,
  STAGE_TICKS_DB,
  advance,
  grNorm,
  levelNorm,
  linearToDb,
  pushHistory,
  shouldTagPeak,
  silentSample,
} from './meter'

describe('stage level scale', () => {
  test('maps 0 dBFS to the top and the floor to the bottom', () => {
    expect(levelNorm(STAGE_CEIL_DB)).toBe(0)
    expect(levelNorm(STAGE_FLOOR_DB)).toBe(1)
  })

  test('is monotonic — louder is always higher on the face', () => {
    let previous = -Infinity
    for (let db = STAGE_FLOOR_DB; db <= STAGE_CEIL_DB; db += 1) {
      const height = 1 - levelNorm(db)
      expect(height).toBeGreaterThan(previous)
      previous = height
    }
  })

  test('clamps rather than running off the face', () => {
    expect(levelNorm(12)).toBe(0)
    expect(levelNorm(-96)).toBe(1)
  })
})

describe('gain reduction / clip overlay', () => {
  test('grows downward from the top as reduction increases', () => {
    expect(grNorm(0)).toBe(0)
    expect(grNorm(GR_FULL_SCALE_DB)).toBe(1)
    expect(grNorm(GR_FULL_SCALE_DB / 2)).toBeCloseTo(0.5, 9)
  })

  test('clamps deep spikes', () => {
    expect(grNorm(-1)).toBe(0)
    expect(grNorm(100)).toBe(1)
  })
})

describe('stage ticks', () => {
  test('are inside the declared range and include the endpoints', () => {
    expect(STAGE_TICKS_DB[0]).toBe(STAGE_CEIL_DB)
    expect(STAGE_TICKS_DB.at(-1)).toBe(STAGE_FLOOR_DB)
    for (const tick of STAGE_TICKS_DB) {
      expect(tick).toBeLessThanOrEqual(STAGE_CEIL_DB)
      expect(tick).toBeGreaterThanOrEqual(STAGE_FLOOR_DB)
    }
  })
})

describe('history ring', () => {
  test('grows until capacity then drops the oldest', () => {
    let history = [silentSample()]
    for (let i = 0; i < HISTORY_LENGTH + 5; i++) {
      history = pushHistory(history, {
        inDb: -i,
        rmsDb: -i,
        outDb: -i,
        grDb: i % 4,
      })
    }
    expect(history.length).toBe(HISTORY_LENGTH)
    expect(history.at(-1)?.inDb).toBe(-(HISTORY_LENGTH + 4))
    expect(history[0]?.inDb).toBe(-5)
  })

  test('a silent sample rests at the stage floor with no reduction', () => {
    const sample = silentSample()
    expect(sample.inDb).toBe(STAGE_FLOOR_DB)
    expect(sample.grDb).toBe(0)
  })
})

describe('peak tags', () => {
  test('only fire on rising GR above the threshold', () => {
    expect(shouldTagPeak(0.1, 0)).toBe(false)
    expect(shouldTagPeak(0.5, 0.2)).toBe(true)
    expect(shouldTagPeak(0.5, 0.45)).toBe(false)
  })
})

describe('meter ballistics', () => {
  test('never overshoots and holds still without time or a target', () => {
    expect(advance(0.5, 1, 0)).toBe(0.5)
    expect(advance(0.5, Number.NaN, 0.1)).toBe(0.5)
    let value = 0
    for (let i = 0; i < 10_000; i++) value = advance(value, 1, 0.01)
    expect(value).toBeLessThanOrEqual(1)
  })
})

describe('level conversion', () => {
  test('unity is 0 dB and silence floors instead of returning -Infinity', () => {
    expect(linearToDb(1)).toBeCloseTo(0, 9)
    expect(linearToDb(0.5)).toBeCloseTo(-6.02, 2)
    expect(Number.isFinite(linearToDb(0))).toBe(true)
  })
})
