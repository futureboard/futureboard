import { describe, expect, test } from 'bun:test'
import {
  DEFAULT_PARAMS,
  FACTORY_PRESETS,
  matchingPresetIndex,
  paramsMatch,
} from './presets'
import { DIVISION_BEATS, divisionMs } from './params'
import { PARAMS } from './params'

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
      expect(entry.params.freeze).toBe(false)
      for (const division of [entry.params.divisionL, entry.params.divisionR]) {
        expect(Number.isInteger(division)).toBe(true)
        expect(division).toBeGreaterThanOrEqual(0)
        expect(division).toBeLessThan(DIVISION_BEATS.length)
      }
      // `link` promises the two sides agree — Rust snaps them together on the
      // way in, so a preset that says otherwise would not survive the trip.
      if (entry.params.link) {
        expect(entry.params.timeMsL).toBe(entry.params.timeMsR)
        expect(entry.params.divisionL).toBe(entry.params.divisionR)
      }
    }
  })

  /** A synced preset's free times are the fallback the user lands on when they
   *  switch Sync off, so they have to state the same figure at 120 BPM. */
  test('a synced preset restates its own divisions in its free times', () => {
    const synced = FACTORY_PRESETS.filter((entry) => entry.params.sync)
    expect(synced.length).toBeGreaterThan(0)
    for (const entry of synced) {
      expect(divisionMs(entry.params.divisionL, 120)).toBeCloseTo(
        entry.params.timeMsL,
        6,
      )
      expect(divisionMs(entry.params.divisionR, 120)).toBeCloseTo(
        entry.params.timeMsR,
        6,
      )
    }
  })

  test('matchingPresetIndex recognizes Default and rejects edits', () => {
    expect(matchingPresetIndex(DEFAULT_PARAMS)).toBe(0)
    expect(
      matchingPresetIndex({ ...DEFAULT_PARAMS, mix: DEFAULT_PARAMS.mix + 1 }),
    ).toBeNull()
  })
})
