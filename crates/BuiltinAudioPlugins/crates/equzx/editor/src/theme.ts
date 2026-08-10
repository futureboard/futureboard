/**
 * The palette, for the places CSS can't reach — canvas paints and SVG fills.
 * Keep these in step with the @theme block in index.css; that file owns the
 * same values for everything expressed as a Tailwind class.
 */

/** Hot signal pink. Anything actively shaping the sound is drawn in this. */
export const NEON = '#ff4d9d'
export const NEON_RGB = '255,77,157'

/** The soft pastel end of the same hue — highlights, solo, secondary text. */
export const MOCHI = '#ffd3e4'
export const MOCHI_RGB = '255,211,228'

/** Neutral surfaces. Hue-free on purpose, so the pink is the only colour. */
export const SURFACE_DEEP = '#0b0b0d'
export const SURFACE_HUB = '#17171a'

/**
 * Per-band accents. One hue, alternating pale and vivid so that neighbouring
 * bands stay tellable apart without breaking the monochrome scheme.
 */
export const BAND_COLORS = [
  '#ffe1ee',
  '#ff4d9d',
  '#ffb3d1',
  '#ff2e8b',
  '#ffd0e2',
  '#ff6fb0',
  '#ff90c0',
  '#e81f77',
  '#ffc4da',
  '#ff5aa3',
  '#ffa0c8',
  '#ff7ab5',
]
