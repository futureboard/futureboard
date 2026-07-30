import { describe, expect, test } from 'bun:test'
import { BOOLEAN_KEYS, NUMERIC_KEYS, defaults } from './bridge'
import {
  FACTORY_PRESETS,
  matchingPresetIndex,
  paramsMatch,
} from './presets'

describe('MixStation factory presets', () => {
  test('contain complete finite parameter states', () => {
    expect(FACTORY_PRESETS.length).toBeGreaterThan(3)
    for (const preset of FACTORY_PRESETS) {
      for (const key of BOOLEAN_KEYS) {
        expect(typeof preset.params[key]).toBe('boolean')
      }
      for (const key of NUMERIC_KEYS) {
        expect(Number.isFinite(preset.params[key])).toBe(true)
      }
    }
  })

  test('default is the first matching preset', () => {
    expect(paramsMatch(defaults, FACTORY_PRESETS[0]!.params)).toBe(true)
    expect(matchingPresetIndex({ ...defaults })).toBe(0)
    expect(FACTORY_PRESETS[0]!.name).toBe('Empty Rack')
    expect(
      BOOLEAN_KEYS.filter((key) => key !== 'power').every(
        (key) => defaults[key] === false,
      ),
    ).toBe(true)
  })

  test('reports modified settings as custom', () => {
    expect(matchingPresetIndex({ ...defaults, widthPct: 101 })).toBeNull()
  })
})
