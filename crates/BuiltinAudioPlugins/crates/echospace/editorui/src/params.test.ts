import { describe, expect, test } from 'bun:test'

import {
  DEFAULT_DIVISION_L,
  DEFAULT_DIVISION_R,
  DEFAULT_TEMPO_BPM,
  DIVISION_BEATS,
  DIVISION_LABELS,
  MAX_TEMPO_BPM,
  MIN_TEMPO_BPM,
  MODES,
  MODE_LABELS,
  PARAMS,
  divisionMs,
  format,
  fromNorm,
  modeFromWire,
  modeToWire,
  toNorm,
  unitFor,
  type ParamId,
} from './params'
import {
  IPC_RS,
  LIB_RS,
  rustFloatArray,
  rustModeOrder,
  rustRanges,
  rustScalar,
  rustStringArray,
  rustU8Scalar,
} from './rust'

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
   * The flags, the mode and the two note divisions are not knobs, so they are
   * absent from `PARAMS` on purpose — but nothing else may be.
   */
  test('only the non-continuous params are missing from the knob table', () => {
    const missing = rustIds.filter((id) => !(id in PARAMS))
    expect(missing.sort()).toEqual([
      'divisionL',
      'divisionR',
      'freeze',
      'link',
      'mode',
      'power',
      'sync',
    ])
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

describe('note divisions mirror echospace::DIVISION_*', () => {
  test('the label table matches the Rust table entry for entry', () => {
    expect(DIVISION_LABELS.map(String)).toEqual(
      rustStringArray(LIB_RS, 'DIVISION_LABELS'),
    )
  })

  /** The index is the wire value, so a beat count that drifts would silently
   *  play a different note than the one the control reads out. */
  test('the beat counts match the Rust table entry for entry', () => {
    const rustBeats = rustFloatArray(LIB_RS, 'DIVISION_BEATS')
    expect(DIVISION_BEATS).toHaveLength(rustBeats.length)
    for (const [index, beats] of DIVISION_BEATS.entries()) {
      expect(beats).toBeCloseTo(rustBeats[index]!, 6)
    }
  })

  test('the tables are the same length and the wire ceiling is the last index', () => {
    expect(DIVISION_LABELS).toHaveLength(DIVISION_BEATS.length)
    expect(rustScalar(LIB_RS, 'MAX_DIVISION_WIRE')).toBe(
      DIVISION_BEATS.length - 1,
    )
  })

  test('durations rise monotonically, so stepping sweeps the range', () => {
    for (let i = 1; i < DIVISION_BEATS.length; i++) {
      expect(DIVISION_BEATS[i]!).toBeGreaterThan(DIVISION_BEATS[i - 1]!)
    }
  })

  test('the tempo window and the defaults match Rust', () => {
    expect(MIN_TEMPO_BPM).toBe(rustScalar(LIB_RS, 'MIN_TEMPO_BPM'))
    expect(MAX_TEMPO_BPM).toBe(rustScalar(LIB_RS, 'MAX_TEMPO_BPM'))
    expect(DEFAULT_TEMPO_BPM).toBe(rustScalar(LIB_RS, 'DEFAULT_TEMPO_BPM'))
    expect(DEFAULT_DIVISION_L).toBe(rustU8Scalar(LIB_RS, 'DEFAULT_DIVISION_L'))
    expect(DEFAULT_DIVISION_R).toBe(rustU8Scalar(LIB_RS, 'DEFAULT_DIVISION_R'))
  })

  test('a quarter note is one beat and the dotted forms are 1.5x', () => {
    expect(divisionMs(DIVISION_LABELS.indexOf('1/4'), 120)).toBeCloseTo(500, 6)
    expect(divisionMs(DIVISION_LABELS.indexOf('1/8'), 120)).toBeCloseTo(250, 6)
    expect(divisionMs(DIVISION_LABELS.indexOf('1/8.'), 120)).toBeCloseTo(375, 6)
    expect(divisionMs(DIVISION_LABELS.indexOf('1/4T'), 120)).toBeCloseTo(
      1000 / 3,
      6,
    )
    // Halving the tempo doubles every length.
    expect(divisionMs(DIVISION_LABELS.indexOf('1/4'), 60)).toBeCloseTo(1000, 6)
  })

  /** The defaults are chosen so switching Sync on at 120 BPM lands on the same
   *  figure the free-time defaults describe. */
  test('the default divisions restate the default free times at 120 BPM', () => {
    expect(divisionMs(DEFAULT_DIVISION_L, 120)).toBeCloseTo(
      PARAMS.timeMsL.default,
      6,
    )
    expect(divisionMs(DEFAULT_DIVISION_R, 120)).toBeCloseTo(500, 6)
  })

  test('a garbage index or tempo clamps instead of producing NaN', () => {
    const longest = DIVISION_BEATS.length - 1
    expect(divisionMs(-5, 120)).toBe(divisionMs(0, 120))
    expect(divisionMs(999, 120)).toBe(divisionMs(longest, 120))
    expect(divisionMs(10, Number.NaN)).toBe(divisionMs(10, DEFAULT_TEMPO_BPM))
    expect(divisionMs(10, 0)).toBe(divisionMs(10, MIN_TEMPO_BPM))
    expect(divisionMs(10, 1e9)).toBe(divisionMs(10, MAX_TEMPO_BPM))
  })

  /** The longest divisions run past the delay line's ceiling at slow tempos;
   *  the readout has to show the length that is actually reachable. */
  test('a division longer than the line clamps to its maximum', () => {
    expect(divisionMs(DIVISION_LABELS.indexOf('1/1.'), 40)).toBe(
      PARAMS.timeMsL.max,
    )
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
