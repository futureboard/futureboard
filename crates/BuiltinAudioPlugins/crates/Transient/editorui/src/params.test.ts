import { describe, expect, test } from 'bun:test'

import { PARAMS, format, fromNorm, toNorm, type ParamId } from './params'
import { IPC_RS, rustRanges, rustStringArray } from './rust'

const rustIds = rustStringArray(IPC_RS, 'UI_PARAM_IDS')
const rustRangeTable = rustRanges('RANGES')

describe('the schema mirrors transient::ipc', () => {
  test('every editor param id exists in the Rust wire table', () => {
    for (const id of Object.keys(PARAMS) as ParamId[]) {
      expect(rustIds).toContain(id)
    }
  })

  test('ranges match the Rust ranges exactly', () => {
    for (const id of Object.keys(PARAMS) as ParamId[]) {
      const spec = PARAMS[id]
      const index = rustIds.indexOf(id)
      const range = rustRangeTable[index]
      expect(range, `no Rust range for ${id}`).toBeDefined()
      expect([id, spec.min, spec.max]).toEqual([id, range![0], range![1]])
      expect(spec.default).toBeGreaterThanOrEqual(spec.min)
      expect(spec.default).toBeLessThanOrEqual(spec.max)
    }
  })

  test('only the discrete controls are missing from the knob table', () => {
    const missing = rustIds.filter((id) => !(id in PARAMS))
    expect(missing.sort()).toEqual(['power', 'stereoLink'])
  })
})

describe('control tapers', () => {
  test('every taper round-trips across its whole range', () => {
    for (const id of Object.keys(PARAMS) as ParamId[]) {
      const spec = PARAMS[id]
      for (const t of [0, 0.13, 0.5, 0.87, 1]) {
        const value = fromNorm(spec, t)
        expect(value).toBeGreaterThanOrEqual(spec.min - 1e-6)
        expect(value).toBeLessThanOrEqual(spec.max + 1e-6)
        expect(toNorm(spec, value)).toBeCloseTo(t, 5)
      }
    }
  })

  test('the endpoints are exact, not a few ulps short', () => {
    for (const id of Object.keys(PARAMS) as ParamId[]) {
      expect(fromNorm(PARAMS[id], 0)).toBe(PARAMS[id].min)
      expect(fromNorm(PARAMS[id], 1)).toBe(PARAMS[id].max)
    }
  })
})

describe('readouts', () => {
  test('bipolar percentages carry a sign and plain ones stay plain', () => {
    expect(format(PARAMS.attack, 40)).toBe('+40')
    expect(format(PARAMS.attack, -25)).toBe('-25')
    expect(format(PARAMS.sustain, 0)).toBe('0')
    expect(format(PARAMS.mix, 100)).toBe('100')
    expect(format(PARAMS.speed, 50)).toBe('50')
  })

  test('a value outside the range is displayed clamped, never as NaN', () => {
    expect(format(PARAMS.mix, 1e9)).toBe('100')
    expect(format(PARAMS.mix, -50)).toBe('0')
    expect(format(PARAMS.attack, 200)).toBe('+100')
  })
})
