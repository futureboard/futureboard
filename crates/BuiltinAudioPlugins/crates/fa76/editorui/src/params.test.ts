import { describe, expect, test } from 'bun:test'

import {
  RATIOS,
  RATIO_LABELS,
  PARAMS,
  format,
  fromNorm,
  ratioFromWire,
  ratioToWire,
  toNorm,
  type ParamId,
} from './params'
import { IPC_RS, rustRatioOrder, rustRanges, rustStringArray } from './rust'

const rustIds = rustStringArray(IPC_RS, 'UI_PARAM_IDS')
const rustRangeTable = rustRanges('RANGES')

describe('the schema mirrors fa76::ipc', () => {
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

  test('only power and ratio are missing from the knob table', () => {
    const missing = rustIds.filter((id) => !(id in PARAMS))
    expect(missing.sort()).toEqual(['power', 'ratio'])
  })

  test('ratio order is the wire contract', () => {
    expect(RATIOS.map(String)).toEqual(rustRatioOrder())
    for (const [index, ratio] of RATIOS.entries()) {
      expect(ratioToWire(ratio)).toBe(index)
      expect(ratioFromWire(index)).toBe(ratio)
      expect(RATIO_LABELS[ratio]).toBeTruthy()
    }
  })

  test('an unknown ratio wire value falls back to r4, as Rust does', () => {
    expect(ratioFromWire(99)).toBe('r4')
    expect(ratioFromWire(-1)).toBe('r4')
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

  test('attack uses a geometric taper', () => {
    const spec = PARAMS.attackUs
    expect(fromNorm(spec, 0.5)).toBeCloseTo(Math.sqrt(spec.min * spec.max), 3)
  })
})

describe('readouts', () => {
  test('gains carry a sign and levels stay plain', () => {
    expect(format(PARAMS.inputDb, 6)).toBe('+6.0')
    expect(format(PARAMS.inputDb, -3)).toBe('-3.0')
    expect(format(PARAMS.mix, 35)).toBe('35')
    expect(format(PARAMS.sidechainHpfHz, 60)).toBe('60')
  })

  test('a value outside the range is displayed clamped, never as NaN', () => {
    expect(format(PARAMS.mix, 1e9)).toBe('100')
    expect(format(PARAMS.mix, -50)).toBe('0')
  })
})
