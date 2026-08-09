import { useMemo, useState } from 'react'
import { scaleLinear, scaleLog } from 'd3-scale'
import { area as d3area, curveBasis, curveMonotoneX, line as d3line } from 'd3-shape'
import {
  DISPLAY_SAMPLE_RATE,
  chainMagnitudeDb,
  compressorOutputDb,
  eqSections,
  filterSections,
  saturate,
  stereoWidth,
} from './response'
import { useMeasure } from './useMeasure'
import { clamp, type ParamSpec } from './params'
import type { MixStationParams } from './bridge'

const F_MIN = 20
const F_MAX = 20_000
const EQ_RANGE_DB = 18

/** Shared frame: hairline grid, one accent trace, no decorative fill beyond data. */
function Plot({
  children,
  label,
  className = '',
}: {
  children: React.ReactNode
  label: string
  className?: string
}) {
  return (
    <div
      role="img"
      aria-label={label}
      className={`overflow-hidden rounded border border-hairline bg-well ${className}`}
    >
      {children}
    </div>
  )
}

const logTicks = [100, 1_000, 10_000]

/** Combined 24 dB/oct cut response, drawn from the same coefficients Rust uses. */
export function FilterCurve({
  hpfHz,
  lpfHz,
  accent,
  active,
}: {
  hpfHz: number
  lpfHz: number
  accent: string
  active: boolean
}) {
  const [ref, { w, h }] = useMeasure<HTMLDivElement>()

  const path = useMemo(() => {
    const x = scaleLog().domain([F_MIN, F_MAX]).range([0, w])
    const y = scaleLinear().domain([-30, 6]).range([h - 1, 1])
    const sections = filterSections(hpfHz, lpfHz, DISPLAY_SAMPLE_RATE)
    const points: [number, number][] = []
    for (let i = 0; i <= 120; i++) {
      const f = F_MIN * Math.pow(F_MAX / F_MIN, i / 120)
      points.push([
        x(f),
        y(clamp(chainMagnitudeDb(sections, DISPLAY_SAMPLE_RATE, f), -30, 6)),
      ])
    }
    const generator = d3line<[number, number]>()
      .x((d) => d[0])
      .y((d) => d[1])
      .curve(curveBasis)
    return { d: generator(points) ?? '', x, y }
  }, [hpfHz, lpfHz, w, h])

  return (
    <Plot label="High and low cut response" className="h-16 flex-1 min-w-32">
      <div ref={ref} className="h-full w-full">
        <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className="h-full w-full">
          {logTicks.map((f) => (
            <line
              key={f}
              x1={path.x(f)}
              x2={path.x(f)}
              y1={0}
              y2={h}
              stroke="rgb(255 255 255 / 0.05)"
              vectorEffect="non-scaling-stroke"
            />
          ))}
          <line
            x1={0}
            x2={w}
            y1={path.y(0)}
            y2={path.y(0)}
            stroke="rgb(255 255 255 / 0.1)"
            vectorEffect="non-scaling-stroke"
          />
          <path
            d={path.d}
            fill="none"
            stroke={active ? accent : 'var(--color-ink-dim)'}
            strokeWidth={1.75}
            strokeLinejoin="round"
            vectorEffect="non-scaling-stroke"
          />
        </svg>
      </div>
    </Plot>
  )
}

type EqBand = 'low' | 'lowMid' | 'highMid' | 'high'

const EQ_NODES: {
  band: EqBand
  gainId: 'lowGainDb' | 'lowMidGainDb' | 'highMidGainDb' | 'highGainDb'
  freqId?: 'lowMidFreqHz' | 'highMidFreqHz'
  /** Shelf corners are fixed in Rust, so those nodes only move vertically. */
  fixedHz?: number
  colour: string
}[] = [
  { band: 'low', gainId: 'lowGainDb', fixedHz: 100, colour: '#60a5fa' },
  { band: 'lowMid', gainId: 'lowMidGainDb', freqId: 'lowMidFreqHz', colour: '#4ade80' },
  { band: 'highMid', gainId: 'highMidGainDb', freqId: 'highMidFreqHz', colour: '#f472b6' },
  { band: 'high', gainId: 'highGainDb', fixedHz: 10_000, colour: '#f0b45e' },
]

/**
 * Four-band EQ response with draggable band nodes.
 *
 * The shelves sit at the fixed corners `lib::apply_params` builds them with, so
 * their nodes move in gain only; the mids are sweepable in both axes. Dragging
 * writes straight through to the bridge, and the curve is the true summed
 * magnitude of the four biquads rather than a stylised approximation.
 */
export function EqCurve({
  params,
  specs,
  accent,
  active,
  onChange,
}: {
  params: MixStationParams
  specs: Record<string, ParamSpec>
  accent: string
  active: boolean
  onChange: (id: string, value: number) => void
}) {
  const [ref, { w, h }] = useMeasure<HTMLDivElement>()
  const [dragBand, setDragBand] = useState<EqBand | null>(null)

  const x = useMemo(() => scaleLog().domain([F_MIN, F_MAX]).range([0, w]), [w])
  const y = useMemo(
    () => scaleLinear().domain([-EQ_RANGE_DB, EQ_RANGE_DB]).range([h, 0]),
    [h],
  )

  const { curve, fill } = useMemo(() => {
    const sections = eqSections(params, DISPLAY_SAMPLE_RATE)
    const points: [number, number][] = []
    for (let i = 0; i <= 128; i++) {
      const f = F_MIN * Math.pow(F_MAX / F_MIN, i / 128)
      points.push([
        x(f),
        y(
          clamp(
            chainMagnitudeDb(sections, DISPLAY_SAMPLE_RATE, f),
            -EQ_RANGE_DB,
            EQ_RANGE_DB,
          ),
        ),
      ])
    }
    const lineGen = d3line<[number, number]>()
      .x((d) => d[0])
      .y((d) => d[1])
      .curve(curveBasis)
    const areaGen = d3area<[number, number]>()
      .x((d) => d[0])
      .y0(y(0))
      .y1((d) => d[1])
      .curve(curveBasis)
    return { curve: lineGen(points) ?? '', fill: areaGen(points) ?? '' }
  }, [params, x, y])

  const onMove = (event: React.PointerEvent<SVGGElement>) => {
    if (!dragBand || !ref.current) return
    const node = EQ_NODES.find((item) => item.band === dragBand)
    if (!node) return
    const rect = ref.current.getBoundingClientRect()
    const gainSpec = specs[node.gainId]
    if (gainSpec) {
      const py = clamp(event.clientY - rect.top, 0, rect.height)
      onChange(node.gainId, clamp(y.invert(py), gainSpec.min, gainSpec.max))
    }
    if (node.freqId) {
      const freqSpec = specs[node.freqId]
      if (freqSpec) {
        const px = clamp(event.clientX - rect.left, 0, rect.width)
        onChange(node.freqId, clamp(x.invert(px), freqSpec.min, freqSpec.max))
      }
    }
  }

  return (
    <Plot
      label="Equaliser response — drag a band node to set its gain, and frequency for the sweepable mids"
      className="h-[76px] flex-1 min-w-40"
    >
      <div ref={ref} className="relative h-full w-full">
        <svg
          viewBox={`0 0 ${w} ${h}`}
          preserveAspectRatio="none"
          className="h-full w-full touch-none"
        >
          {logTicks.map((f) => (
            <line
              key={f}
              x1={x(f)}
              x2={x(f)}
              y1={0}
              y2={h}
              stroke="rgb(255 255 255 / 0.05)"
              vectorEffect="non-scaling-stroke"
            />
          ))}
          <line
            x1={0}
            x2={w}
            y1={y(0)}
            y2={y(0)}
            stroke="rgb(255 255 255 / 0.1)"
            vectorEffect="non-scaling-stroke"
          />
          {/* Accents arrive as CSS custom properties, so tint via color-mix. */}
          <path
            d={fill}
            fill={active ? `color-mix(in srgb, ${accent} 14%, transparent)` : 'transparent'}
          />
          <path
            d={curve}
            fill="none"
            stroke={active ? accent : 'var(--color-ink-dim)'}
            strokeWidth={1.75}
            strokeLinejoin="round"
            vectorEffect="non-scaling-stroke"
          />

          {EQ_NODES.map((node) => {
            const gain = params[node.gainId]
            const freq = node.fixedHz ?? params[node.freqId!]
            const focused = dragBand === node.band
            return (
              <g
                key={node.band}
                transform={`translate(${x(freq)} ${y(clamp(gain, -EQ_RANGE_DB, EQ_RANGE_DB))})`}
                className={node.fixedHz ? 'cursor-ns-resize' : 'cursor-move'}
                onPointerDown={(event) => {
                  event.currentTarget.setPointerCapture(event.pointerId)
                  setDragBand(node.band)
                }}
                onPointerMove={onMove}
                onPointerUp={(event) => {
                  event.currentTarget.releasePointerCapture(event.pointerId)
                  setDragBand(null)
                }}
                onPointerCancel={() => setDragBand(null)}
                onDoubleClick={() => onChange(node.gainId, 0)}
              >
                {/* Oversized transparent hit area — the visible dot stays small. */}
                <circle r={11} fill="transparent" />
                <circle
                  r={focused ? 5.5 : 4}
                  fill={node.colour}
                  stroke="var(--color-well)"
                  strokeWidth={1.5}
                />
              </g>
            )
          })}
        </svg>
      </div>
    </Plot>
  )
}

/** Static compressor transfer curve, including the fixed 6 dB soft knee. */
export function CompressorCurve({
  thresholdDb,
  ratio,
  makeupDb,
  accent,
  active,
}: {
  thresholdDb: number
  ratio: number
  makeupDb: number
  accent: string
  active: boolean
}) {
  const [ref, { w, h }] = useMeasure<HTMLDivElement>()

  const { d, x, y } = useMemo(() => {
    const xs = scaleLinear().domain([-60, 0]).range([0, w])
    const ys = scaleLinear().domain([-60, 6]).range([h, 0])
    const points: [number, number][] = []
    for (let db = -60; db <= 0; db += 1) {
      points.push([
        xs(db),
        ys(clamp(compressorOutputDb(db, thresholdDb, ratio, makeupDb), -60, 6)),
      ])
    }
    const generator = d3line<[number, number]>()
      .x((p) => p[0])
      .y((p) => p[1])
      .curve(curveMonotoneX)
    return { d: generator(points) ?? '', x: xs, y: ys }
  }, [thresholdDb, ratio, makeupDb, w, h])

  return (
    <Plot label="Compressor transfer curve" className="h-16 w-40 shrink-0">
      <div ref={ref} className="h-full w-full">
        <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className="h-full w-full">
          {[-50, -40, -30, -20, -10].map((db) => (
            <line
              key={db}
              x1={x(db)}
              x2={x(db)}
              y1={0}
              y2={h}
              stroke="rgb(255 255 255 / 0.05)"
              vectorEffect="non-scaling-stroke"
            />
          ))}
          {/* Unity reference so the amount of reduction is readable at a glance. */}
          <line
            x1={x(-60)}
            y1={y(-60)}
            x2={x(0)}
            y2={y(0)}
            stroke="rgb(255 255 255 / 0.12)"
            strokeDasharray="2 3"
            vectorEffect="non-scaling-stroke"
          />
          <line
            x1={x(thresholdDb)}
            x2={x(thresholdDb)}
            y1={0}
            y2={h}
            stroke={active ? accent : 'var(--color-ink-dim)'}
            strokeOpacity={0.4}
            vectorEffect="non-scaling-stroke"
          />
          <path
            d={d}
            fill="none"
            stroke={active ? accent : 'var(--color-ink-dim)'}
            strokeWidth={1.75}
            vectorEffect="non-scaling-stroke"
          />
        </svg>
      </div>
    </Plot>
  )
}

/**
 * Saturation transfer curve.
 *
 * Drawn through the same `saturate` maths as the audio path, so the visible
 * asymmetry at the extremes of Character is the actual even-harmonic bias
 * rather than an illustration of it.
 */
export function SaturationCurve({
  drivePct,
  characterPct,
  accent,
  active,
}: {
  drivePct: number
  characterPct: number
  accent: string
  active: boolean
}) {
  const [ref, { w, h }] = useMeasure<HTMLDivElement>()

  const { d, x, y } = useMemo(() => {
    const xs = scaleLinear().domain([-1.2, 1.2]).range([0, w])
    const ys = scaleLinear().domain([-1.2, 1.2]).range([h, 0])
    const points: [number, number][] = []
    for (let i = 0; i <= 96; i++) {
      const input = -1.2 + (2.4 * i) / 96
      points.push([xs(input), ys(clamp(saturate(input, drivePct, characterPct), -1.2, 1.2))])
    }
    const generator = d3line<[number, number]>()
      .x((p) => p[0])
      .y((p) => p[1])
      .curve(curveMonotoneX)
    return { d: generator(points) ?? '', x: xs, y: ys }
  }, [drivePct, characterPct, w, h])

  return (
    <Plot label="Saturation transfer curve" className="h-16 w-24 shrink-0">
      <div ref={ref} className="h-full w-full">
        <svg viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" className="h-full w-full">
          <line
            x1={x(-1.2)}
            y1={y(-1.2)}
            x2={x(1.2)}
            y2={y(1.2)}
            stroke="rgb(255 255 255 / 0.1)"
            strokeDasharray="2 3"
            vectorEffect="non-scaling-stroke"
          />
          <line
            x1={x(0)}
            x2={x(0)}
            y1={0}
            y2={h}
            stroke="rgb(255 255 255 / 0.05)"
            vectorEffect="non-scaling-stroke"
          />
          <line
            x1={0}
            x2={w}
            y1={y(0)}
            y2={y(0)}
            stroke="rgb(255 255 255 / 0.05)"
            vectorEffect="non-scaling-stroke"
          />
          <path
            d={d}
            fill="none"
            stroke={active ? accent : 'var(--color-ink-dim)'}
            strokeWidth={1.75}
            vectorEffect="non-scaling-stroke"
          />
        </svg>
      </div>
    </Plot>
  )
}

/**
 * Stereo width read-out.
 *
 * With no analysis data on the bridge this cannot show programme material, so
 * it shows the transform itself: where a hard-panned pair lands after
 * `dsp::stereo_width` at the current setting. Honest, and it still moves with
 * the control.
 */
export function WidthGraphic({
  widthPct,
  accent,
  active,
}: {
  widthPct: number
  accent: string
  active: boolean
}) {
  const [ref, { w, h }] = useMeasure<HTMLDivElement>()
  const width = widthPct * 0.01
  const [left, right] = stereoWidth(1, -1, width)
  const stroke = active ? accent : 'var(--color-ink-dim)'
  const cx = w / 2
  const cy = h / 2
  const span = (w / 2 - 10) * clamp(Math.abs(left), 0, 2) * 0.5

  return (
    <Plot label="Stereo width transform" className="h-16 w-28 shrink-0">
      <div ref={ref} className="h-full w-full">
        <svg viewBox={`0 0 ${w} ${h}`} className="h-full w-full">
          <line
            x1={cx}
            x2={cx}
            y1={4}
            y2={h - 4}
            stroke="rgb(255 255 255 / 0.08)"
            vectorEffect="non-scaling-stroke"
          />
          <line
            x1={cx - span}
            x2={cx + span}
            y1={cy}
            y2={cy}
            stroke={stroke}
            strokeWidth={2}
            strokeLinecap="round"
            vectorEffect="non-scaling-stroke"
          />
          <circle cx={cx - span} cy={cy} r={3} fill={stroke} />
          <circle cx={cx + span} cy={cy} r={3} fill={stroke} />
          <text
            x={cx}
            y={h - 5}
            textAnchor="middle"
            className="readout"
            fill="var(--color-ink-dim)"
            fontSize={9}
          >
            {width === 0 ? 'mono' : right > 0 ? 'inverted' : 'stereo'}
          </text>
        </svg>
      </div>
    </Plot>
  )
}

