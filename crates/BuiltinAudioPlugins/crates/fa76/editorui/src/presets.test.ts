import { describe, expect, test } from 'bun:test'
import {
  DEFAULT_PARAMS,
  FACTORY_PRESETS,
  matchingPresetIndex,
  paramsMatch,
} from './presets'
import { PARAMS, RATIOS } from './params'

describe('factory presets', () => {
  test('Default mirrors the schema defaults', () => {
    expect(FACTORY_PRESETS[0]?.name).toBe('Default')
    expect(paramsMatch(FACTORY_PRESETS[0]!.params, DEFAULT_PARAMS)).toBe(true)
    for (const id of Object.keys(PARAMS) as (keyof typeof PARAMS)[]) {
      expect(DEFAULT_PARAMS[id]).toBe(PARAMS[id].default)
    }
  })

  test('every preset stays inside the schema ranges', () => {
    for (const entry of FACTORY_PRESETS) {
      for (const id of Object.keys(PARAMS) as (keyof typeof PARAMS)[]) {
        const spec = PARAMS[id]
        const value = entry.params[id]
        expect(value).toBeGreaterThanOrEqual(spec.min)
        expect(value).toBeLessThanOrEqual(spec.max)
      }
      expect(entry.params.power).toBe(true)
      expect(RATIOS.includes(entry.params.ratio)).toBe(true)
    }
  })

  test('matchingPresetIndex recognizes Default and rejects edits', () => {
    expect(matchingPresetIndex(DEFAULT_PARAMS)).toBe(0)
    expect(
      matchingPresetIndex({ ...DEFAULT_PARAMS, mix: DEFAULT_PARAMS.mix + 1 }),
    ).toBeNull()
  })

  test('preset names are unique', () => {
    const names = FACTORY_PRESETS.map((entry) => entry.name)
    expect(new Set(names).size).toBe(names.length)
  })
})
