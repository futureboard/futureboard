import { describe, expect, test } from 'bun:test'
import { NUMERIC_KEYS, defaults } from './bridge'
import {
  PARAM_SPECS,
  normalizedValue,
  sanitizeParams,
  valueFromNormalized,
} from './params'

describe('MixStation parameter mirror', () => {
  test('defines one spec for every numeric wire parameter', () => {
    expect(Object.keys(PARAM_SPECS).sort()).toEqual([...NUMERIC_KEYS].sort())
    for (const id of NUMERIC_KEYS) {
      expect(PARAM_SPECS[id].defaultValue).toBe(defaults[id])
    }
  })

  test('clamps incoming values to the declared wire ranges', () => {
    const clean = sanitizeParams({
      ...defaults,
      inputTrimDb: 999,
      compRatio: -3,
      widthPct: 400,
      limiterCeilingDb: 6,
    })
    expect(clean.inputTrimDb).toBe(24)
    expect(clean.compRatio).toBe(1)
    expect(clean.widthPct).toBe(200)
    expect(clean.limiterCeilingDb).toBe(0)
  })

  test('round-trips logarithmic controls', () => {
    const specification = PARAM_SPECS.hpfHz
    const normalized = normalizedValue(specification, 100)
    expect(valueFromNormalized(specification, normalized)).toBe(100)
  })
})
