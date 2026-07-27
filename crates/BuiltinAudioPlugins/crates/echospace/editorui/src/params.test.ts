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
  unitFor,
  type ParamId,
} from './params'
import { IPC_RS, rustModeOrder, rustRanges, rustStringArray } from './rust'

const rustIds = rustStringArray(IPC_RS, 'UI_PARAM_IDS')
const rustRangeTable = rustRanges('RANGES')

describe('the schema mirrors echospace::ipc', () => {
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

  /**
   * `power`, `mode` and `freeze` are not knobs, so they are absent from
   * `PARAMS` on purpose — but nothing else may be.
   */
  test('only the non-continuous params are missing from the knob table', () => {
    const missing = rustIds.filter((id) => !(id in PARAMS))
    expect(missing.sort()).toEqual(['freeze', 'mode', 'power'])
  })

  test('mode order is the wire contract', () => {
    expect(MODES.map(String)).toEqual(rustModeOrder())
    for (const [index, mode] of MODES.entries()) {
      expect(modeToWire(mode)).toBe(index)
      expect(modeFromWire(index)).toBe(mode)
      expect(MODE_LABELS[mode]).toBeTruthy()
    }
  })

  test('an unknown mode wire value falls back to ping-pong, as Rust does', () => {
    expect(modeFromWire(99)).toBe('pingpong')
    expect(modeFromWire(-1)).toBe('pingpong')
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

  test('out-of-range travel clamps instead of extrapolating', () => {
    const spec = PARAMS.timeMsL
    expect(fromNorm(spec, -3)).toBe(spec.min)
    expect(fromNorm(spec, 4)).toBe(spec.max)
    expect(toNorm(spec, -100)).toBe(0)
    expect(toNorm(spec, 1e9)).toBe(1)
  })

  /** Exponential tapers must put the midpoint at the geometric mean — that is
   *  the whole reason a delay time does not use a linear dial. */
  test('time and frequency tapers are geometric', () => {
    for (const spec of [PARAMS.timeMsL, PARAMS.highCutHz, PARAMS.lowCutHz]) {
      expect(fromNorm(spec, 0.5)).toBeCloseTo(Math.sqrt(spec.min * spec.max), 3)
    }
  })
})

describe('readouts', () => {
  test('delay times flip to seconds and frequencies to kHz', () => {
    expect(format(PARAMS.timeMsL, 375)).toBe('375')
    expect(unitFor(PARAMS.timeMsL, 375)).toBe('ms')
    expect(format(PARAMS.timeMsL, 1500)).toBe('1.50')
    expect(unitFor(PARAMS.timeMsL, 1500)).toBe('s')
    expect(format(PARAMS.highCutHz, 9000)).toBe('9.00k')
    expect(format(PARAMS.lowCutHz, 180)).toBe('180')
  })

  test('gains carry a sign', () => {
    expect(format(PARAMS.outputDb, 3)).toBe('+3.0')
    expect(format(PARAMS.outputDb, -6)).toBe('-6.0')
  })

  test('a value outside the range is displayed clamped, never as NaN', () => {
    expect(format(PARAMS.mix, 1e9)).toBe('100')
    expect(format(PARAMS.mix, -50)).toBe('0')
  })
})
