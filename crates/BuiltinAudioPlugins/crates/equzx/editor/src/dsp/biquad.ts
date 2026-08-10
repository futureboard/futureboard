/**
 * Biquad coefficient generation + magnitude response.
 *
 * Formulas follow the Web Audio API BiquadFilterNode spec (RBJ cookbook), so the
 * curve we draw is the exact response of the filters we actually run the audio
 * through — no approximation drift between the display and what you hear.
 */

export interface Coeffs {
  b0: number
  b1: number
  b2: number
  a1: number
  a2: number
}

const SQRT2 = Math.SQRT2

/** Normalize by a0 so evaluation is a single pass. */
function norm(b0: number, b1: number, b2: number, a0: number, a1: number, a2: number): Coeffs {
  return { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
}

export function peaking(freq: number, q: number, gainDb: number, sr: number): Coeffs {
  const A = Math.pow(10, gainDb / 40)
  const w0 = (2 * Math.PI * freq) / sr
  const cw = Math.cos(w0)
  const alpha = Math.sin(w0) / (2 * q)
  return norm(1 + alpha * A, -2 * cw, 1 - alpha * A, 1 + alpha / A, -2 * cw, 1 - alpha / A)
}

export function lowShelf(freq: number, gainDb: number, sr: number): Coeffs {
  const A = Math.pow(10, gainDb / 40)
  const w0 = (2 * Math.PI * freq) / sr
  const cw = Math.cos(w0)
  // S = 1 => alpha = sin(w0)/2 * sqrt(2)
  const alpha = (Math.sin(w0) / 2) * SQRT2
  const twoSqrtAAlpha = 2 * Math.sqrt(A) * alpha
  return norm(
    A * (A + 1 - (A - 1) * cw + twoSqrtAAlpha),
    2 * A * (A - 1 - (A + 1) * cw),
    A * (A + 1 - (A - 1) * cw - twoSqrtAAlpha),
    A + 1 + (A - 1) * cw + twoSqrtAAlpha,
    -2 * (A - 1 + (A + 1) * cw),
    A + 1 + (A - 1) * cw - twoSqrtAAlpha,
  )
}

export function highShelf(freq: number, gainDb: number, sr: number): Coeffs {
  const A = Math.pow(10, gainDb / 40)
  const w0 = (2 * Math.PI * freq) / sr
  const cw = Math.cos(w0)
  const alpha = (Math.sin(w0) / 2) * SQRT2
  const twoSqrtAAlpha = 2 * Math.sqrt(A) * alpha
  return norm(
    A * (A + 1 + (A - 1) * cw + twoSqrtAAlpha),
    -2 * A * (A - 1 + (A + 1) * cw),
    A * (A + 1 + (A - 1) * cw - twoSqrtAAlpha),
    A + 1 - (A - 1) * cw + twoSqrtAAlpha,
    2 * (A - 1 - (A + 1) * cw),
    A + 1 - (A - 1) * cw - twoSqrtAAlpha,
  )
}

export function notch(freq: number, q: number, sr: number): Coeffs {
  const w0 = (2 * Math.PI * freq) / sr
  const cw = Math.cos(w0)
  const alpha = Math.sin(w0) / (2 * q)
  return norm(1, -2 * cw, 1, 1 + alpha, -2 * cw, 1 - alpha)
}

export function bandpass(freq: number, q: number, sr: number): Coeffs {
  const w0 = (2 * Math.PI * freq) / sr
  const cw = Math.cos(w0)
  const alpha = Math.sin(w0) / (2 * q)
  return norm(alpha, 0, -alpha, 1 + alpha, -2 * cw, 1 - alpha)
}

/**
 * Web Audio's lowpass/highpass take Q in *decibels*, unlike the other types.
 * We pass linear Q around everywhere and convert here so a Butterworth stage
 * (Q = 0.7071) lands where it should.
 */
export function lowpass(freq: number, qLinear: number, sr: number): Coeffs {
  const w0 = (2 * Math.PI * freq) / sr
  const cw = Math.cos(w0)
  const alpha = Math.sin(w0) / (2 * qLinear)
  const oneMinusCw = 1 - cw
  return norm(oneMinusCw / 2, oneMinusCw, oneMinusCw / 2, 1 + alpha, -2 * cw, 1 - alpha)
}

export function highpass(freq: number, qLinear: number, sr: number): Coeffs {
  const w0 = (2 * Math.PI * freq) / sr
  const cw = Math.cos(w0)
  const alpha = Math.sin(w0) / (2 * qLinear)
  const onePlusCw = 1 + cw
  return norm(onePlusCw / 2, -onePlusCw, onePlusCw / 2, 1 + alpha, -2 * cw, 1 - alpha)
}

/** |H(e^jw)| for a normalized biquad, evaluated at frequency `f`. */
export function magnitude(c: Coeffs, f: number, sr: number): number {
  const w = (2 * Math.PI * f) / sr
  const cw = Math.cos(w)
  const c2w = Math.cos(2 * w)
  const sw = Math.sin(w)
  const s2w = Math.sin(2 * w)

  const numRe = c.b0 + c.b1 * cw + c.b2 * c2w
  const numIm = -(c.b1 * sw + c.b2 * s2w)
  const denRe = 1 + c.a1 * cw + c.a2 * c2w
  const denIm = -(c.a1 * sw + c.a2 * s2w)

  const num = Math.hypot(numRe, numIm)
  const den = Math.hypot(denRe, denIm)
  return den === 0 ? 0 : num / den
}

/**
 * Precomputed trig for a fixed set of frequencies. Dynamic bands re-evaluate their
 * response every frame, and the sin/cos calls dominate that cost — hoisting them
 * out turns each magnitude lookup into plain arithmetic.
 */
export interface ResponseGrid {
  freqs: Float64Array
  cw: Float64Array
  sw: Float64Array
  c2w: Float64Array
  s2w: Float64Array
}

export function makeGrid(freqs: ArrayLike<number>, sr: number): ResponseGrid {
  const n = freqs.length
  const grid: ResponseGrid = {
    freqs: new Float64Array(n),
    cw: new Float64Array(n),
    sw: new Float64Array(n),
    c2w: new Float64Array(n),
    s2w: new Float64Array(n),
  }
  for (let i = 0; i < n; i++) {
    const w = (2 * Math.PI * freqs[i]) / sr
    grid.freqs[i] = freqs[i]
    grid.cw[i] = Math.cos(w)
    grid.sw[i] = Math.sin(w)
    grid.c2w[i] = Math.cos(2 * w)
    grid.s2w[i] = Math.sin(2 * w)
  }
  return grid
}

/** |H| at grid point `i`. Equivalent to `magnitude`, without the trig. */
export function magnitudeAt(c: Coeffs, g: ResponseGrid, i: number): number {
  const cw = g.cw[i]
  const sw = g.sw[i]
  const c2w = g.c2w[i]
  const s2w = g.s2w[i]

  const numRe = c.b0 + c.b1 * cw + c.b2 * c2w
  const numIm = -(c.b1 * sw + c.b2 * s2w)
  const denRe = 1 + c.a1 * cw + c.a2 * c2w
  const denIm = -(c.a1 * sw + c.a2 * s2w)

  const num = Math.sqrt(numRe * numRe + numIm * numIm)
  const den = Math.sqrt(denRe * denRe + denIm * denIm)
  return den === 0 ? 0 : num / den
}

/**
 * Butterworth Q values for the second-order sections of an even-order cascade.
 * order 2 -> [0.7071], order 4 -> [0.5412, 1.3066], etc.
 */
export function butterworthQs(order: number): number[] {
  const qs: number[] = []
  for (let k = 0; k < order / 2; k++) {
    qs.push(1 / (2 * Math.cos(((2 * k + 1) * Math.PI) / (2 * order))))
  }
  return qs
}
