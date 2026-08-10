/**
 * Turning FFT bins into pixel columns.
 *
 * The analyser's bins are linearly spaced in frequency while the display is
 * logarithmic, so the two ends of the spectrum need opposite treatment: down at
 * 20 Hz many pixel columns fall inside one bin (interpolate), and up at 20 kHz
 * many bins fall inside one column (aggregate).
 */

/** Reusable per-column buffers so the draw loop allocates nothing per frame. */
export interface SpectrumScratch {
  cols: number
  raw: Float32Array
  tmp: Float32Array
  smooth: Float32Array
}

export function makeScratch(): SpectrumScratch {
  return { cols: 0, raw: new Float32Array(0), tmp: new Float32Array(0), smooth: new Float32Array(0) }
}

export function ensureScratch(s: SpectrumScratch, cols: number) {
  if (s.cols === cols) return
  s.cols = cols
  s.raw = new Float32Array(cols)
  s.tmp = new Float32Array(cols)
  s.smooth = new Float32Array(cols)
}

function idx(i: number, n: number) {
  return i < 0 ? 0 : i >= n ? n - 1 : i
}

/**
 * Catmull-Rom sample of the bin array at a fractional index, clamped to the two
 * samples it sits between so the spline can't ring past them.
 *
 * This is what removes the low-end staircase: FFT bins are linearly spaced while
 * the display is logarithmic, so near 20 Hz a whole run of pixel columns maps
 * inside a single bin. Reading that bin per column draws a flat step; sampling
 * *between* bins draws the curve the data implies.
 */
export function sampleBins(bins: Float32Array, pos: number, floorDb: number): number {
  const n = bins.length
  const i = Math.floor(pos)
  const t = pos - i
  const at = (k: number) => {
    const v = bins[idx(k, n)]
    return Number.isFinite(v) ? Math.max(v, floorDb) : floorDb
  }
  const p0 = at(i - 1)
  const p1 = at(i)
  const p2 = at(i + 1)
  const p3 = at(i + 2)

  const v =
    0.5 *
    (2 * p1 +
      (-p0 + p2) * t +
      (2 * p0 - 5 * p1 + 4 * p2 - p3) * t * t +
      (-p0 + 3 * p1 - 3 * p2 + p3) * t * t * t)

  return Math.min(Math.max(v, Math.min(p1, p2)), Math.max(p1, p2))
}

/** O(n) box blur via a running sum, with clamped edges. */
export function boxBlur(src: Float32Array, dst: Float32Array, n: number, r: number) {
  if (r <= 0) {
    dst.set(src.subarray(0, n))
    return
  }
  const win = 2 * r + 1
  let sum = 0
  for (let i = -r; i <= r; i++) sum += src[idx(i, n)]
  for (let i = 0; i < n; i++) {
    dst[i] = sum / win
    sum -= src[idx(i - r, n)]
    sum += src[idx(i + r + 1, n)]
  }
}
