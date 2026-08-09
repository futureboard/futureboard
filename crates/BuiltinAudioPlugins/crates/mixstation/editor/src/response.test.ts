import { describe, expect, test } from 'bun:test'
import {
  COMP_KNEE_DB,
  DISPLAY_SAMPLE_RATE,
  HPF_OPEN_HZ,
  LPF_OPEN_HZ,
  SATURATION_DRIVE_SCALE,
  chainMagnitudeDb,
  compressorGainDb,
  eqSections,
  filterSections,
  proportionalQ,
  saturate,
  stereoWidth,
} from './response'

/**
 * These lock the editor's display maths to the assertions in the Rust suite.
 * If a curve here drifts from `src/dsp.rs`, the editor would be drawing a
 * filter the audio path does not run — which is exactly the failure the tests
 * exist to catch.
 */

/** Panel drive percent that yields the Rust tests' internal drive of 4.0. */
const DRIVE_PCT_FOR_4 = 4 / SATURATION_DRIVE_SCALE
const CENTRED_CHARACTER_PCT = 50

describe('cut filters', () => {
  // Mirrors `dsp::fourth_order_high_pass_is_three_db_down_at_its_corner`.
  test('fourth-order high pass is 3 dB down at its corner', () => {
    const sections = filterSections(200, LPF_OPEN_HZ, DISPLAY_SAMPLE_RATE)
    const db = chainMagnitudeDb(sections, DISPLAY_SAMPLE_RATE, 200)
    expect(Math.abs(db + 3)).toBeLessThan(0.6)
  })

  // Mirrors `dsp::butterworth_cut_bypasses_at_the_ends_of_its_range`.
  test('parked filters leave the path entirely', () => {
    const sections = filterSections(HPF_OPEN_HZ, LPF_OPEN_HZ, DISPLAY_SAMPLE_RATE)
    for (const f of [40, 200, 1_000, 8_000]) {
      expect(Math.abs(chainMagnitudeDb(sections, DISPLAY_SAMPLE_RATE, f))).toBeLessThan(1e-6)
    }
  })

  test('high pass actually attenuates below its corner', () => {
    const sections = filterSections(300, LPF_OPEN_HZ, DISPLAY_SAMPLE_RATE)
    expect(chainMagnitudeDb(sections, DISPLAY_SAMPLE_RATE, 40)).toBeLessThan(-24)
  })
})

describe('equaliser', () => {
  const flat = {
    lowGainDb: 0,
    lowMidFreqHz: 400,
    lowMidGainDb: 0,
    highMidFreqHz: 2_500,
    highMidGainDb: 0,
    highGainDb: 0,
  }

  test('a flat EQ draws a flat line', () => {
    const sections = eqSections(flat, DISPLAY_SAMPLE_RATE)
    for (const f of [30, 120, 1_000, 6_000, 15_000]) {
      expect(Math.abs(chainMagnitudeDb(sections, DISPLAY_SAMPLE_RATE, f))).toBeLessThan(1e-4)
    }
  })

  test('a mid bell reaches its set gain at its centre frequency', () => {
    const sections = eqSections({ ...flat, lowMidGainDb: 9 }, DISPLAY_SAMPLE_RATE)
    expect(chainMagnitudeDb(sections, DISPLAY_SAMPLE_RATE, 400)).toBeCloseTo(9, 1)
  })

  test('the low shelf is pinned at 100 Hz and reaches full gain below it', () => {
    const sections = eqSections({ ...flat, lowGainDb: 6 }, DISPLAY_SAMPLE_RATE)
    // Half gain at the corner is the defining property of an RBJ shelf.
    expect(chainMagnitudeDb(sections, DISPLAY_SAMPLE_RATE, 100)).toBeCloseTo(3, 0)
    expect(chainMagnitudeDb(sections, DISPLAY_SAMPLE_RATE, 25)).toBeCloseTo(6, 0)
  })

  // Mirrors `dsp::proportional_q_tightens_with_gain`.
  test('proportional Q matches the Rust curve', () => {
    expect(proportionalQ(0)).toBeCloseTo(0.7, 6)
    expect(proportionalQ(18)).toBeGreaterThan(proportionalQ(6))
    expect(proportionalQ(-18)).toBeGreaterThan(proportionalQ(0))
    expect(proportionalQ(40)).toBeLessThanOrEqual(1.9 + 1e-6)
  })
})

describe('compressor curve', () => {
  test('no reduction below the knee', () => {
    expect(compressorGainDb(-40, -20, 4)).toBe(0)
  })

  // Mirrors `dsp::compressor_reaches_the_static_curve`: -6 dB into a -20 dB
  // threshold at 4:1 settles at 10.5 dB of reduction.
  test('reaches the documented static reduction', () => {
    expect(compressorGainDb(-6, -20, 4)).toBeCloseTo(-10.5, 6)
  })

  test('the knee is continuous at both boundaries', () => {
    const half = COMP_KNEE_DB / 2
    const belowKnee = compressorGainDb(-20 - half - 1e-6, -20, 4)
    const atKneeStart = compressorGainDb(-20 - half + 1e-6, -20, 4)
    expect(Math.abs(belowKnee - atKneeStart)).toBeLessThan(1e-4)

    const insideKnee = compressorGainDb(-20 + half - 1e-6, -20, 4)
    const pastKnee = compressorGainDb(-20 + half + 1e-6, -20, 4)
    expect(Math.abs(insideKnee - pastKnee)).toBeLessThan(1e-4)
  })

  test('a 1:1 ratio never reduces', () => {
    for (const db of [-40, -10, 0]) expect(compressorGainDb(db, -20, 1)).toBeCloseTo(0, 6)
  })
})

describe('saturation', () => {
  // Mirrors `dsp::saturation_meets_the_bypassed_path_at_zero_drive`.
  test('zero drive is a straight wire', () => {
    for (const x of [-0.9, -0.3, 0.2, 0.75]) {
      expect(saturate(x, 0, 70)).toBe(x)
    }
  })

  // Mirrors `dsp::saturation_is_level_matched_and_smooth`.
  test('the curve is level matched across character settings', () => {
    for (const characterPct of [0, 25, 50, 75, 100]) {
      const positive = saturate(1, DRIVE_PCT_FOR_4, characterPct)
      const negative = saturate(-1, DRIVE_PCT_FOR_4, characterPct)
      expect(Math.abs((positive - negative) * 0.5 - 1)).toBeLessThan(1e-3)
      expect(Math.abs(saturate(0, DRIVE_PCT_FOR_4, characterPct))).toBeLessThan(1e-6)
    }
  })

  // Mirrors `dsp::centred_character_stays_symmetric`.
  test('centred character is exactly symmetric', () => {
    for (const x of [0.1, 0.4, 0.9]) {
      const positive = saturate(x, DRIVE_PCT_FOR_4, CENTRED_CHARACTER_PCT)
      const negative = saturate(-x, DRIVE_PCT_FOR_4, CENTRED_CHARACTER_PCT)
      expect(Math.abs(positive + negative)).toBeLessThan(1e-6)
    }
  })

  test('the curve is monotonic, so the plot never doubles back', () => {
    let previous = Number.NEGATIVE_INFINITY
    for (let step = -100; step <= 100; step++) {
      const value = saturate(step / 100, DRIVE_PCT_FOR_4, 80)
      expect(value).toBeGreaterThan(previous)
      previous = value
    }
  })

  test('bias off-centre breaks symmetry — the even harmonics the plot shows', () => {
    const positive = saturate(0.8, DRIVE_PCT_FOR_4, 100)
    const negative = saturate(-0.8, DRIVE_PCT_FOR_4, 100)
    expect(Math.abs(positive + negative)).toBeGreaterThan(1e-3)
  })
})

describe('stereo width', () => {
  // Mirrors `dsp::width_preserves_mid_and_scales_side`.
  test('preserves mid and scales side', () => {
    expect(stereoWidth(0.5, -0.5, 0)).toEqual([0, 0])
    expect(stereoWidth(0.5, -0.5, 2)).toEqual([1, -1])
    expect(stereoWidth(0.25, 0.25, 2)).toEqual([0.25, 0.25])
  })
})
