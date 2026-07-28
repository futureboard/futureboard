import { describe, expect, test } from 'bun:test'
import {
  PARAMS,
  STYLES,
  STYLE_LABELS,
  format,
  fromNorm,
  styleFromWire,
  styleToWire,
  toNorm,
  type ParamId,
} from './params'
import { IPC_RS, rustRanges, rustStringArray, rustStyleOrder } from './rust'

const rustIds = rustStringArray(IPC_RS, 'UI_PARAM_IDS')
const rustRangeTable = rustRanges('RANGES')

describe('the schema mirrors burnlimit::ipc', () => {
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
    }
  })

  test('non-knob wire ids are the discrete controls', () => {
    const missing = rustIds.filter((id) => !(id in PARAMS))
    expect(missing.sort()).toEqual(['power', 'stereoLink', 'style', 'truePeak'])
  })

  test('style order is the wire contract', () => {
    expect(STYLES.map(String)).toEqual(rustStyleOrder())
    for (const [index, style] of STYLES.entries()) {
      expect(styleToWire(style)).toBe(index)
      expect(styleFromWire(index)).toBe(style)
      expect(STYLE_LABELS[style]).toBeTruthy()
    }
  })
})

describe('control tapers', () => {
  test('every taper round-trips across its whole range', () => {
    for (const id of Object.keys(PARAMS) as ParamId[]) {
      const spec = PARAMS[id]
      for (const t of [0, 0.13, 0.5, 0.87, 1]) {
        const value = fromNorm(spec, t)
        expect(toNorm(spec, value)).toBeCloseTo(t, 5)
      }
    }
  })

  test('the endpoints are exact', () => {
    for (const id of Object.keys(PARAMS) as ParamId[]) {
      expect(fromNorm(PARAMS[id], 0)).toBe(PARAMS[id].min)
      expect(fromNorm(PARAMS[id], 1)).toBe(PARAMS[id].max)
    }
  })
})

describe('readouts', () => {
  test('gains carry a sign', () => {
    expect(format(PARAMS.gainDb, 6)).toBe('+6.0')
    expect(format(PARAMS.gainDb, -3)).toBe('-3.0')
    expect(format(PARAMS.ceilingDb, -0.3)).toBe('-0.3')
  })
})
