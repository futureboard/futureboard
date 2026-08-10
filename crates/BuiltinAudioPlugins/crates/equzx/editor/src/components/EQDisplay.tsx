import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import * as d3 from 'd3'
import { animate } from 'animejs'
import type { AudioEngine } from '../audio/AudioEngine'
import { makeGrid, type ResponseGrid } from '../dsp/biquad'
import { MOCHI, MOCHI_RGB, NEON_RGB, SURFACE_DEEP } from '../theme'
import { boxBlur, ensureScratch, makeScratch, sampleBins, type SpectrumScratch } from '../dsp/spectrum'
import {
  IS_CUT,
  USES_GAIN,
  USES_Q,
  bandColor,
  bandInView,
  bandSections,
  sectionsDbAt,
  type Band,
  type ChannelView,
} from '../dsp/bands'

export type AnalyzerMode = 'off' | 'pre' | 'post' | 'both'

interface Props {
  bands: Band[]
  selectedId: number | null
  soloId: number | null
  bypassed: boolean
  dbRange: number
  analyzerMode: AnalyzerMode
  /** Fractional-octave spectrum smoothing, e.g. 1/12. 0 = raw bins. */
  spectrumSmoothing: number
  /** Which slice of the stereo image is on screen; bands outside it are hidden. */
  channelView: ChannelView
  engine: AudioEngine | null
  canAdd: boolean
  onPatch: (id: number, patch: Partial<Band>) => void
  onSelect: (id: number | null) => void
  onSolo: (id: number | null) => void
  onAdd: (freq: number, gain: number) => void
  onRemove: (id: number) => void
}

const F_MIN = 20
const F_MAX = 22000
const PAD = { top: 14, right: 14, bottom: 26, left: 40 }
const CURVE_POINTS = 480

const FREQ_TICKS = [
  20, 30, 40, 50, 60, 80, 100, 200, 300, 400, 500, 600, 800, 1000, 2000, 3000, 4000, 5000,
  6000, 8000, 10000, 20000,
]
const LABELLED = new Set([20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000])

/** Pink-noise tilt so a full mix reads roughly flat across the display. */
const TILT_DB_PER_OCT = 4.5

function fmtFreq(f: number): string {
  if (f >= 10000) return `${(f / 1000).toFixed(0)}k`
  if (f >= 1000) return `${(f / 1000).toFixed(f % 1000 === 0 ? 0 : 1)}k`
  return `${Math.round(f)}`
}

/** Q <-> bandwidth in octaves (RBJ relation). */
function qToOctaves(q: number): number {
  return (2 / Math.LN2) * Math.asinh(1 / (2 * q))
}
function octavesToQ(bw: number): number {
  return 1 / (2 * Math.sinh((Math.LN2 / 2) * Math.max(bw, 0.02)))
}

function clamp(v: number, lo: number, hi: number) {
  return Math.min(Math.max(v, lo), hi)
}

/**
 * Static bands are evaluated once per edit; dynamic bands change every frame, so
 * they're kept apart and re-summed on top of the cached static total.
 */
interface CurveCache {
  grid: ResponseGrid
  staticTotal: Float64Array
  staticPerBand: Map<number, Float64Array>
  dynamic: { band: Band; index: number }[]
  scratchTotal: Float64Array
  scratchBand: Float64Array
}

export function EQDisplay({
  bands,
  selectedId,
  soloId,
  bypassed,
  dbRange,
  analyzerMode,
  spectrumSmoothing,
  channelView,
  engine,
  canAdd,
  onPatch,
  onSelect,
  onSolo,
  onAdd,
  onRemove,
}: Props) {
  const wrapRef = useRef<HTMLDivElement>(null)
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const svgRef = useRef<SVGSVGElement>(null)
  const [size, setSize] = useState({ w: 900, h: 460 })
  const [hoverId, setHoverId] = useState<number | null>(null)
  const [cursor, setCursor] = useState<{ x: number; y: number } | null>(null)
  const justDragged = useRef(false)

  /**
   * Right-button drag on a handle: solo the band for as long as the button is
   * held, sweeping its frequency as you move. Whatever was soloed before comes
   * back on release, so this stays a momentary audition rather than a toggle.
   */
  const audition = useRef<{
    id: number
    pointerId: number
    prevSolo: number | null
    startX: number
    startFreq: number
  } | null>(null)

  const endAudition = useCallback(() => {
    const a = audition.current
    if (!a) return
    audition.current = null
    onSolo(a.prevSolo)
  }, [onSolo])

  const sr = engine?.sampleRate ?? 48000

  const inner = {
    w: Math.max(10, size.w - PAD.left - PAD.right),
    h: Math.max(10, size.h - PAD.top - PAD.bottom),
  }
  const x = useMemo(() => d3.scaleLog().domain([F_MIN, F_MAX]).range([0, inner.w]), [inner.w])
  const y = useMemo(
    () => d3.scaleLinear().domain([-dbRange, dbRange]).range([inner.h, 0]),
    [inner.h, dbRange],
  )

  const live = useRef({
    bands, x, y, inner, analyzerMode, bypassed, selectedId, hoverId, soloId, sr, spectrumSmoothing,
  })
  useEffect(() => {
    live.current = {
      bands, x, y, inner, analyzerMode, bypassed, selectedId, hoverId, soloId, sr, spectrumSmoothing,
    }
  })

  // --- resize ------------------------------------------------------------
  useEffect(() => {
    const el = wrapRef.current
    if (!el) return
    const ro = new ResizeObserver(([entry]) => {
      const { width, height } = entry.contentRect
      setSize({ w: Math.round(width), h: Math.round(height) })
    })
    ro.observe(el)
    return () => ro.disconnect()
  }, [])

  // --- curve cache -------------------------------------------------------
  const cache = useRef<CurveCache | null>(null)

  const gridFreqs = useMemo(() => {
    const freqs = new Float64Array(CURVE_POINTS + 1)
    for (let i = 0; i <= CURVE_POINTS; i++) {
      freqs[i] = F_MIN * Math.pow(F_MAX / F_MIN, i / CURVE_POINTS)
    }
    return freqs
  }, [])

  useEffect(() => {
    const n = gridFreqs.length
    const grid = makeGrid(gridFreqs, sr)
    const staticTotal = new Float64Array(n)
    const staticPerBand = new Map<number, Float64Array>()
    const dynamic: { band: Band; index: number }[] = []

    bands.forEach((band, index) => {
      if (!bandInView(band, channelView)) return
      const isDyn = band.dynamic && band.enabled && USES_GAIN[band.type] && !bypassed
      if (isDyn) {
        dynamic.push({ band, index })
        return
      }
      const sections = bandSections(band, sr)
      const curve = new Float64Array(n)
      for (let i = 0; i < n; i++) curve[i] = sectionsDbAt(sections, grid, i)
      staticPerBand.set(band.id, curve)
      if (band.enabled && !bypassed) {
        for (let i = 0; i < n; i++) staticTotal[i] += curve[i]
      }
    })

    cache.current = {
      grid,
      staticTotal,
      staticPerBand,
      dynamic,
      scratchTotal: new Float64Array(n),
      scratchBand: new Float64Array(n),
    }
  }, [bands, sr, bypassed, gridFreqs, channelView])

  // --- render loop -------------------------------------------------------
  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas) return
    const ctx = canvas.getContext('2d')
    if (!ctx) return

    const dpr = Math.min(window.devicePixelRatio || 1, 2)
    canvas.width = Math.round(size.w * dpr)
    canvas.height = Math.round(size.h * dpr)
    canvas.style.width = `${size.w}px`
    canvas.style.height = `${size.h}px`

    const binsPre = new Float32Array(engine?.preAnalyser.frequencyBinCount ?? 1024)
    const binsPost = new Float32Array(engine?.postAnalyser.frequencyBinCount ?? 1024)
    let peaks: Float32Array | null = null
    const scratchPre = makeScratch()
    const scratchPost = makeScratch()
    let raf = 0

    const draw = () => {
      raf = requestAnimationFrame(draw)
      const L = live.current
      ctx.save()
      ctx.scale(dpr, dpr)
      ctx.clearRect(0, 0, size.w, size.h)
      ctx.translate(PAD.left, PAD.top)

      drawGrid(ctx, L.x, L.y, L.inner, dbRange)

      if (engine && L.analyzerMode !== 'off') {
        if (L.analyzerMode !== 'post') {
          engine.preAnalyser.getFloatFrequencyData(binsPre)
          peaks = drawSpectrum(
            ctx, binsPre, engine.preAnalyser, L.x, L.inner, sr,
            {
              // Pre stays neutral grey; only the processed signal gets colour.
              fill: 'rgba(150,150,155,0.14)',
              stroke: 'rgba(180,180,188,0.30)',
              peaks,
              hold: true,
              octaveFraction: L.spectrumSmoothing,
            },
            scratchPre,
          )
        }
        if (L.analyzerMode !== 'pre') {
          engine.postAnalyser.getFloatFrequencyData(binsPost)
          drawSpectrum(
            ctx, binsPost, engine.postAnalyser, L.x, L.inner, sr,
            {
              fill: `rgba(${NEON_RGB},0.13)`,
              stroke: `rgba(${MOCHI_RGB},0.55)`,
              peaks: null,
              hold: false,
              octaveFraction: L.spectrumSmoothing,
            },
            scratchPost,
          )
        }
      }

      if (cache.current) drawCurves(ctx, cache.current, gridFreqs, L, engine, sr)
      ctx.restore()
    }
    raf = requestAnimationFrame(draw)
    return () => cancelAnimationFrame(raf)
  }, [size.w, size.h, engine, dbRange, sr, gridFreqs])

  // --- interaction -------------------------------------------------------
  const toLocal = useCallback((ev: { clientX: number; clientY: number }) => {
    const rect = svgRef.current!.getBoundingClientRect()
    return { px: ev.clientX - rect.left - PAD.left, py: ev.clientY - rect.top - PAD.top }
  }, [])

  /** A single click on empty display creates a band there, Pro-Q style. */
  const handleClick = useCallback(
    (ev: React.MouseEvent) => {
      if (justDragged.current) return
      const el = ev.target as Element
      if (el.closest('[data-handle]') || el.closest('[data-qhandle]')) return
      if (!canAdd) return
      const { px, py } = toLocal(ev)
      if (px < 0 || px > inner.w || py < 0 || py > inner.h) return
      onAdd(clamp(x.invert(px), F_MIN, F_MAX), clamp(y.invert(py), -dbRange, dbRange))
    },
    [toLocal, x, y, dbRange, onAdd, canAdd, inner.w, inner.h],
  )

  const handleWheelNative = useCallback(
    (ev: WheelEvent) => {
      const id = hoverId ?? selectedId
      if (id === null) return
      const band = bands.find((b) => b.id === id)
      if (!band) return
      ev.preventDefault()
      const dir = ev.deltaY > 0 ? 1 : -1
      if (IS_CUT[band.type]) {
        const steps = [12, 24, 36, 48, 72, 96]
        const i = steps.indexOf(band.slope)
        onPatch(id, { slope: steps[clamp(i + dir, 0, steps.length - 1)] })
      } else if (USES_Q[band.type]) {
        const factor = ev.shiftKey ? 1.02 : 1.1
        onPatch(id, { q: clamp(band.q * (dir > 0 ? 1 / factor : factor), 0.025, 40) })
      }
    },
    [hoverId, selectedId, bands, onPatch],
  )

  // Native listener: React registers onWheel passively, so preventDefault is ignored there.
  useEffect(() => {
    const svg = svgRef.current
    if (!svg) return
    svg.addEventListener('wheel', handleWheelNative, { passive: false })
    return () => svg.removeEventListener('wheel', handleWheelNative)
  }, [handleWheelNative])

  useEffect(() => {
    const svg = svgRef.current
    if (!svg) return

    const drag = d3
      .drag<SVGGElement, unknown>()
      .on('start', function (ev) {
        onSelect(Number(this.dataset.handle))
        ev.sourceEvent.stopPropagation()
      })
      .on('drag', function (ev) {
        justDragged.current = true
        const id = Number(this.dataset.handle)
        const band = live.current.bands.find((b) => b.id === id)
        if (!band) return
        const fine = ev.sourceEvent.shiftKey ? 0.25 : 1
        const patch: Partial<Band> = {}

        patch.freq = clamp(x.invert(x(band.freq) + ev.dx * fine), F_MIN, F_MAX)
        if (USES_GAIN[band.type] && !ev.sourceEvent.altKey) {
          patch.gain = clamp(y.invert(y(band.gain) + ev.dy * fine), -dbRange, dbRange)
        }
        onPatch(id, patch)
      })

    const qDrag = d3
      .drag<SVGGElement, unknown>()
      .on('drag', function (ev) {
        justDragged.current = true
        const id = Number(this.dataset.qhandle)
        const side = Number(this.dataset.side)
        const band = live.current.bands.find((b) => b.id === id)
        if (!band) return
        const edge = x(band.freq * Math.pow(2, (side * qToOctaves(band.q)) / 2)) + ev.dx
        const octaves = Math.abs(Math.log2(x.invert(edge) / band.freq)) * 2
        onPatch(id, { q: clamp(octavesToQ(octaves), 0.025, 40) })
      })

    d3.select(svg).selectAll<SVGGElement, unknown>('[data-handle]').call(drag)
    d3.select(svg).selectAll<SVGGElement, unknown>('[data-qhandle]').call(qDrag)
  }, [bands, x, y, dbRange, onPatch, onSelect])

  // Pop the newest handle in when a band appears.
  const knownIds = useRef(new Set<number>())
  useEffect(() => {
    const fresh = bands.filter((b) => !knownIds.current.has(b.id))
    knownIds.current = new Set(bands.map((b) => b.id))
    for (const b of fresh) {
      const node = svgRef.current?.querySelector(`[data-handle="${b.id}"] circle`)
      if (node) {
        animate(node, { scale: [0.2, 1], opacity: [0, 1], duration: 420, ease: 'outElastic(1, .6)' })
      }
    }
  }, [bands])

  const focused = bands.find((b) => b.id === (hoverId ?? selectedId)) ?? null
  const hoveredBand = focused && bandInView(focused, channelView) ? focused : null

  return (
    <div ref={wrapRef} className="relative h-full w-full select-none">
      <canvas ref={canvasRef} className="absolute inset-0" />
      <svg
        ref={svgRef}
        className={`absolute inset-0 h-full w-full ${canAdd ? 'cursor-crosshair' : 'cursor-not-allowed'}`}
        onClick={handleClick}
        onContextMenu={(ev) => ev.preventDefault()}
        onPointerDown={() => (justDragged.current = false)}
        onMouseMove={(ev) => {
          const { px, py } = toLocal(ev)
          setCursor({ x: px, y: py })
        }}
        onMouseLeave={() => setCursor(null)}
      >
        <g transform={`translate(${PAD.left},${PAD.top})`}>
          {/* Transparent backdrop so the empty plot area is hit-testable. */}
          <rect width={inner.w} height={inner.h} fill="transparent" />

          {cursor && cursor.x >= 0 && cursor.x <= inner.w && (
            <g pointerEvents="none">
              <line
                x1={cursor.x}
                x2={cursor.x}
                y1={0}
                y2={inner.h}
                stroke="rgba(255,255,255,0.10)"
                strokeDasharray="2 3"
              />
              <text
                x={Math.min(cursor.x + 6, inner.w - 44)}
                y={12}
                className="fill-white/45 text-[10px] tabular-nums"
              >
                {fmtFreq(x.invert(cursor.x))} Hz
              </text>
            </g>
          )}

          {bands.map((band, i) => {
            if (!bandInView(band, channelView)) return null
            const color = bandColor(i)
            const cx = x(band.freq)
            const cy = USES_GAIN[band.type] ? y(band.gain) : y(0)
            const active = selectedId === band.id
            const dim = soloId !== null && soloId !== band.id
            const showQ = active && USES_Q[band.type]
            const bw = qToOctaves(band.q)
            const isDyn = band.dynamic && USES_GAIN[band.type]
            return (
              <g key={band.id} opacity={band.enabled ? (dim ? 0.25 : 1) : 0.3}>
                {active && (
                  <line
                    x1={cx}
                    x2={cx}
                    y1={0}
                    y2={inner.h}
                    stroke={color}
                    strokeOpacity={0.35}
                    strokeDasharray="3 4"
                    pointerEvents="none"
                  />
                )}
                {/* Travel limit of a dynamic band. */}
                {isDyn && (
                  <line
                    x1={cx - 16}
                    x2={cx + 16}
                    y1={y(clamp(band.gain + band.dynRange, -dbRange, dbRange))}
                    y2={y(clamp(band.gain + band.dynRange, -dbRange, dbRange))}
                    stroke={color}
                    strokeOpacity={0.55}
                    strokeDasharray="2 2"
                    pointerEvents="none"
                  />
                )}
                {showQ &&
                  [-1, 1].map((side) => {
                    const qx = x(band.freq * Math.pow(2, (side * bw) / 2))
                    if (!isFinite(qx)) return null
                    return (
                      <g key={side} data-qhandle={band.id} data-side={side} className="cursor-ew-resize">
                        <rect
                          x={qx - 4}
                          y={cy - 4}
                          width={8}
                          height={8}
                          rx={2}
                          fill={SURFACE_DEEP}
                          stroke={color}
                          strokeWidth={1.5}
                        />
                      </g>
                    )
                  })}
                <g
                  data-handle={band.id}
                  className="cursor-grab active:cursor-grabbing"
                  onMouseEnter={() => setHoverId(band.id)}
                  onMouseLeave={() => setHoverId(null)}
                  onPointerDown={(ev) => {
                    if (ev.button !== 2) return
                    ev.preventDefault()
                    ev.stopPropagation()
                    ev.currentTarget.setPointerCapture(ev.pointerId)
                    audition.current = {
                      id: band.id,
                      pointerId: ev.pointerId,
                      prevSolo: soloId,
                      startX: ev.clientX,
                      startFreq: band.freq,
                    }
                    onSelect(band.id)
                    onSolo(band.id)
                  }}
                  onPointerMove={(ev) => {
                    const a = audition.current
                    if (!a || a.pointerId !== ev.pointerId) return
                    justDragged.current = true
                    const fine = ev.shiftKey ? 0.25 : 1
                    const px = x(a.startFreq) + (ev.clientX - a.startX) * fine
                    onPatch(a.id, { freq: clamp(x.invert(px), F_MIN, F_MAX) })
                  }}
                  onPointerUp={endAudition}
                  onPointerCancel={endAudition}
                  onLostPointerCapture={endAudition}
                  onDoubleClick={(ev) => {
                    ev.stopPropagation()
                    onRemove(band.id)
                  }}
                  onClick={(ev) => {
                    ev.stopPropagation()
                    onSelect(band.id)
                  }}
                >
                  <circle cx={cx} cy={cy} r={14} fill="transparent" />
                  {active && <circle cx={cx} cy={cy} r={11} fill="none" stroke={color} strokeOpacity={0.4} />}
                  <circle
                    cx={cx}
                    cy={cy}
                    r={active || hoverId === band.id ? 7 : 5.5}
                    fill={band.enabled ? color : '#20252c'}
                    stroke={SURFACE_DEEP}
                    strokeWidth={1.5}
                    style={{ transition: 'r 120ms ease' }}
                  />
                  {isDyn && (
                    <circle
                      cx={cx}
                      cy={cy}
                      r={10}
                      fill="none"
                      stroke={color}
                      strokeOpacity={0.7}
                      strokeWidth={1}
                      strokeDasharray="1.5 2.5"
                      pointerEvents="none"
                    />
                  )}
                  <text
                    x={cx}
                    y={cy + 3}
                    textAnchor="middle"
                    className="pointer-events-none fill-black/70 text-[8px] font-semibold"
                  >
                    {i + 1}
                  </text>
                  {band.channel !== 'stereo' && (
                    <text
                      x={cx}
                      y={cy - 12}
                      textAnchor="middle"
                      fill={color}
                      className="pointer-events-none text-[9px] font-bold"
                    >
                      {band.channel === 'mid' ? 'M' : 'S'}
                    </text>
                  )}
                </g>
              </g>
            )
          })}
        </g>
      </svg>

      {hoveredBand && (
        <div
          className="pointer-events-none absolute rounded-md border border-white/10 bg-black/80 px-2 py-1 text-[10px] leading-tight tabular-nums text-white/80 shadow-lg backdrop-blur"
          style={{
            left: Math.min(PAD.left + x(hoveredBand.freq) + 14, size.w - 110),
            top: Math.max(4, PAD.top + (USES_GAIN[hoveredBand.type] ? y(hoveredBand.gain) : y(0)) - 40),
          }}
        >
          <div className="font-semibold text-white">
            {fmtFreq(hoveredBand.freq)} Hz
            {hoveredBand.channel !== 'stereo' && (
              <span className="ml-1 font-bold uppercase text-white/45">{hoveredBand.channel}</span>
            )}
          </div>
          {USES_GAIN[hoveredBand.type] ? (
            <div>
              {hoveredBand.gain >= 0 ? '+' : ''}
              {hoveredBand.gain.toFixed(1)} dB
            </div>
          ) : (
            <div>{hoveredBand.slope} dB/oct</div>
          )}
          {USES_Q[hoveredBand.type] && <div>Q {hoveredBand.q.toFixed(2)}</div>}
          {hoveredBand.dynamic && USES_GAIN[hoveredBand.type] && (
            <div className="text-mochi">
              dyn {hoveredBand.dynRange >= 0 ? '+' : ''}
              {hoveredBand.dynRange.toFixed(1)} dB
            </div>
          )}
        </div>
      )}
    </div>
  )
}

// --- canvas painters -----------------------------------------------------

function drawGrid(
  ctx: CanvasRenderingContext2D,
  x: d3.ScaleLogarithmic<number, number>,
  y: d3.ScaleLinear<number, number>,
  inner: { w: number; h: number },
  dbRange: number,
) {
  ctx.lineWidth = 1
  ctx.font = '10px "Mona Sans Variable", "Mona Sans", system-ui, sans-serif'

  for (const f of FREQ_TICKS) {
    const px = Math.round(x(f)) + 0.5
    if (px < 0 || px > inner.w) continue
    ctx.strokeStyle = LABELLED.has(f) ? 'rgba(255,255,255,0.075)' : 'rgba(255,255,255,0.035)'
    ctx.beginPath()
    ctx.moveTo(px, 0)
    ctx.lineTo(px, inner.h)
    ctx.stroke()
    if (LABELLED.has(f)) {
      ctx.fillStyle = 'rgba(255,255,255,0.35)'
      ctx.textAlign = 'center'
      ctx.fillText(fmtFreq(f), px, inner.h + 15)
    }
  }

  const step = dbRange <= 12 ? 3 : 6
  for (let db = -dbRange; db <= dbRange; db += step) {
    const py = Math.round(y(db)) + 0.5
    ctx.strokeStyle = db === 0 ? 'rgba(255,255,255,0.16)' : 'rgba(255,255,255,0.05)'
    ctx.beginPath()
    ctx.moveTo(0, py)
    ctx.lineTo(inner.w, py)
    ctx.stroke()
    ctx.fillStyle = 'rgba(255,255,255,0.32)'
    ctx.textAlign = 'right'
    ctx.fillText(db > 0 ? `+${db}` : `${db}`, -8, py + 3)
  }
}

interface SpectrumOpts {
  fill: string
  stroke: string
  /** Previous frame's peak-hold trace, or null when this layer holds no peaks. */
  peaks: Float32Array | null
  hold: boolean
  /** Fractional-octave smoothing width, e.g. 1/12. 0 disables it. */
  octaveFraction: number
}

/** Draws one analyser's magnitude, tilted so pink noise reads flat. Returns decayed peaks. */
function drawSpectrum(
  ctx: CanvasRenderingContext2D,
  bins: Float32Array,
  analyser: AnalyserNode,
  x: d3.ScaleLogarithmic<number, number>,
  inner: { w: number; h: number },
  sr: number,
  opts: SpectrumOpts,
  scratch: SpectrumScratch,
): Float32Array | null {
  const n = bins.length
  const binHz = sr / 2 / n
  const cols = Math.max(2, Math.round(inner.w))
  const floorDb = analyser.minDecibels

  ensureScratch(scratch, cols)
  const { raw, tmp, smooth } = scratch

  let peaks = opts.peaks
  if (!opts.hold) peaks = null
  else if (!peaks || peaks.length !== cols) peaks = new Float32Array(cols).fill(inner.h)

  // Resolve each pixel column to a dB value, tilted for the pink-noise reference.
  for (let i = 0; i < cols; i++) {
    const fLo = x.invert(i)
    const fHi = x.invert(i + 1)
    const bLo = fLo / binHz
    const bHi = fHi / binHz

    let db: number
    if (bHi - bLo < 1.5) {
      // Sub-bin territory (the low end): interpolate rather than repeat a bin.
      db = sampleBins(bins, (bLo + bHi) / 2, floorDb)
    } else {
      // Several bins per column (the high end): keep the strongest so peaks survive.
      const lo = clamp(Math.floor(bLo), 0, n - 1)
      const hi = clamp(Math.ceil(bHi), lo + 1, n)
      let m = -Infinity
      for (let b = lo; b < hi; b++) if (bins[b] > m) m = bins[b]
      db = Number.isFinite(m) ? Math.max(m, floorDb) : floorDb
    }

    // Ramp the tilt in over the first few dB above the noise floor. Tilting silence
    // itself would lift the top end by ~20 dB and draw a rising diagonal across an
    // empty display instead of a flat floor.
    const fMid = Math.sqrt(fLo * fHi)
    const fade = clamp((db - floorDb) / 6, 0, 1)
    raw[i] = db + fade * TILT_DB_PER_OCT * Math.log2(fMid / 1000)
  }

  // Fractional-octave smoothing. Columns are already log-spaced, so a fixed-width
  // kernel in pixels *is* constant-Q in frequency; two box passes approximate a
  // Gaussian. Smoothing in dB (before mapping to pixels) keeps it level-correct.
  const pxPerOctave = inner.w / Math.log2(F_MAX / F_MIN)
  const radius = opts.octaveFraction > 0
    ? Math.max(1, Math.round((pxPerOctave * opts.octaveFraction) / 2))
    : 0
  let curve = raw
  if (radius > 0) {
    boxBlur(raw, tmp, cols, radius)
    boxBlur(tmp, smooth, cols, radius)
    curve = smooth
  }

  const span = analyser.maxDecibels - analyser.minDecibels
  const toPy = (db: number) => inner.h - clamp((db - analyser.minDecibels) / span, 0, 1.15) * inner.h

  // Fill.
  ctx.beginPath()
  ctx.moveTo(0, inner.h)
  for (let i = 0; i < cols; i++) ctx.lineTo(i, toPy(curve[i]))
  ctx.lineTo(cols - 1, inner.h)
  ctx.closePath()
  ctx.fillStyle = opts.fill
  ctx.fill()

  // Trace.
  ctx.beginPath()
  for (let i = 0; i < cols; i++) {
    const py = toPy(curve[i])
    if (i === 0) ctx.moveTo(i, py)
    else ctx.lineTo(i, py)
    // Smaller py = louder, so a peak is held as a minimum and decays downward.
    if (peaks) peaks[i] = Math.min(py, peaks[i] + 0.5)
  }
  ctx.strokeStyle = opts.stroke
  ctx.lineWidth = 1
  ctx.lineJoin = 'round'
  ctx.stroke()

  if (peaks) {
    ctx.beginPath()
    for (let i = 0; i < cols; i++) {
      if (i === 0) ctx.moveTo(i, peaks[i])
      else ctx.lineTo(i, peaks[i])
    }
    ctx.strokeStyle = 'rgba(214,214,220,0.22)'
    ctx.lineWidth = 1
    ctx.stroke()
  }
  return peaks
}

interface LiveState {
  bands: Band[]
  x: d3.ScaleLogarithmic<number, number>
  y: d3.ScaleLinear<number, number>
  inner: { w: number; h: number }
  selectedId: number | null
  hoverId: number | null
  soloId: number | null
  bypassed: boolean
}

/** Traces a dB array over the frequency grid into the current canvas path. */
function pathFrom(
  ctx: CanvasRenderingContext2D,
  db: Float64Array,
  freqs: Float64Array,
  x: d3.ScaleLogarithmic<number, number>,
  y: d3.ScaleLinear<number, number>,
) {
  ctx.beginPath()
  for (let i = 0; i < db.length; i++) {
    const px = x(freqs[i])
    const py = y(db[i])
    if (i === 0) ctx.moveTo(px, py)
    else ctx.lineTo(px, py)
  }
}

function drawCurves(
  ctx: CanvasRenderingContext2D,
  cache: CurveCache,
  freqs: Float64Array,
  L: LiveState,
  engine: AudioEngine | null,
  sr: number,
) {
  const { x, y, inner } = L
  const n = freqs.length
  const total = cache.scratchTotal
  total.set(cache.staticTotal)

  ctx.save()
  ctx.beginPath()
  ctx.rect(0, 0, inner.w, inner.h)
  ctx.clip()

  const focus = L.hoverId ?? L.selectedId

  // Static band responses.
  L.bands.forEach((band, i) => {
    const curve = cache.staticPerBand.get(band.id)
    if (!curve || !band.enabled || L.bypassed) return
    strokeBand(ctx, curve, freqs, x, y, bandColor(i), focus === band.id)
  })

  // Dynamic bands: recompute at this frame's gain and add into the total.
  for (const { band, index } of cache.dynamic) {
    const delta = engine?.getDelta(band.id) ?? 0
    const sections = bandSections({ ...band, gain: band.gain + delta }, sr)
    const curve = cache.scratchBand
    for (let i = 0; i < n; i++) {
      curve[i] = sectionsDbAt(sections, cache.grid, i)
      total[i] += curve[i]
    }
    strokeBand(ctx, curve, freqs, x, y, bandColor(index), focus === band.id)
  }

  // Composite curve.
  const grad = ctx.createLinearGradient(0, 0, 0, inner.h)
  grad.addColorStop(0, `rgba(${NEON_RGB},0.22)`)
  grad.addColorStop(0.5, `rgba(${NEON_RGB},0.02)`)
  grad.addColorStop(1, `rgba(${NEON_RGB},0.22)`)

  pathFrom(ctx, total, freqs, x, y)
  ctx.lineTo(x(freqs[n - 1]), y(0))
  ctx.lineTo(x(freqs[0]), y(0))
  ctx.closePath()
  ctx.fillStyle = grad
  ctx.fill()

  pathFrom(ctx, total, freqs, x, y)
  // The composite curve is the one true neon element — it gets a real bloom.
  ctx.strokeStyle = L.bypassed ? 'rgba(255,255,255,0.25)' : MOCHI
  ctx.lineWidth = 2
  ctx.shadowColor = `rgba(${NEON_RGB},0.85)`
  ctx.shadowBlur = L.bypassed ? 0 : 14
  ctx.stroke()
  ctx.shadowBlur = 0

  // Live marker showing where each dynamic band currently sits.
  for (const { band, index } of cache.dynamic) {
    const delta = engine?.getDelta(band.id) ?? 0
    const px = x(band.freq)
    const py = y(band.gain + delta)
    ctx.beginPath()
    ctx.moveTo(px, y(band.gain))
    ctx.lineTo(px, py)
    ctx.strokeStyle = bandColor(index)
    ctx.globalAlpha = 0.5
    ctx.lineWidth = 1
    ctx.stroke()
    ctx.globalAlpha = 1
    ctx.beginPath()
    ctx.arc(px, py, 3, 0, Math.PI * 2)
    ctx.fillStyle = bandColor(index)
    ctx.fill()
  }

  ctx.restore()
}

function strokeBand(
  ctx: CanvasRenderingContext2D,
  curve: Float64Array,
  freqs: Float64Array,
  x: d3.ScaleLogarithmic<number, number>,
  y: d3.ScaleLinear<number, number>,
  color: string,
  isFocus: boolean,
) {
  pathFrom(ctx, curve, freqs, x, y)
  ctx.strokeStyle = color
  ctx.globalAlpha = isFocus ? 0.85 : 0.28
  ctx.lineWidth = isFocus ? 1.5 : 1
  ctx.stroke()

  if (isFocus) {
    pathFrom(ctx, curve, freqs, x, y)
    ctx.lineTo(x(freqs[freqs.length - 1]), y(0))
    ctx.lineTo(x(freqs[0]), y(0))
    ctx.closePath()
    ctx.globalAlpha = 0.12
    ctx.fillStyle = color
    ctx.fill()
  }
  ctx.globalAlpha = 1
}
