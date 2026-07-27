import { describe, expect, test } from 'bun:test'

import {
  MODES,
  MODE_LABELS,
  PARAMS,
  format,
  fromNorm,
  modeFromWire,
  modeToWire,
  toNorm,
  type ParamId,
} from './params'
import { IPC_RS, rustModeOrder, rustRanges, rustStringArray } from './rust'

const rustIds = rustStringArray(IPC_RS, 'UI_PARAM_IDS')
const rustRangeTable = rustRanges('RANGES')

describe('the schema mirrors fa2a::ipc', () => {
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

  test('only power and mode are missing from the knob table', () => {
    const missing = rustIds.filter((id) => !(id in PARAMS))
    expect(missing.sort()).toEqual(['mode', 'power'])
  })

  test('mode order is the wire contract', () => {
    expect(MODES.map(String)).toEqual(rustModeOrder())
    for (const [index, mode] of MODES.entries()) {
      expect(modeToWire(mode)).toBe(index)
      expect(modeFromWire(index)).toBe(mode)
      expect(MODE_LABELS[mode]).toBeTruthy()
    }
  })

  test('an unknown mode wire value falls back to compress, as Rust does', () => {
    expect(modeFromWire(99)).toBe('compress')
    expect(modeFromWire(-1)).toBe('compress')
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

  test('the sidechain corner uses a geometric taper', () => {
    const spec = PARAMS.sidechainLowCutHz
    expect(fromNorm(spec, 0.5)).toBeCloseTo(Math.sqrt(spec.min * spec.max), 3)
  })
})

describe('readouts', () => {
  test('gains carry a sign and levels stay plain', () => {
    expect(format(PARAMS.gainDb, 6)).toBe('+6.0')
    expect(format(PARAMS.gainDb, -3)).toBe('-3.0')
    expect(format(PARAMS.peakReduction, 35)).toBe('35')
    expect(format(PARAMS.sidechainLowCutHz, 90)).toBe('90')
  })

  test('a value outside the range is displayed clamped, never as NaN', () => {
    expect(format(PARAMS.mix, 1e9)).toBe('100')
    expect(format(PARAMS.mix, -50)).toBe('0')
  })
})
