import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
  type RefObject,
  type WheelEvent as ReactWheelEvent,
} from 'react'
import type { Band, SpectrumFrame } from '../bridge'
import { SpectrumLayer } from './SpectrumLayer'
import {
  BAND_COLORS,
  GAIN_RANGE,
  GRID_FREQUENCIES,
  GRID_GAINS,
  LABELLED_FREQUENCIES,
  MAX_FREQ,
  MAX_Q,
  MIN_FREQ,
  MIN_Q,
  bandCurvePath,
  bandHasGain,
  clamp,
  filterKind,
  formatFrequency,
  formatGain,
  formatQ,
  frequencyToX,
  gainToY,
  sumCurvePath,
  sumDbAt,
  xToFrequency,
  yToGain,
} from '../lib/eq'

const READOUT_WIDTH = 148
const READOUT_HEIGHT = 30

type Size = { width: number; height: number }

export type ResponseGraphProps = {
  bands: Band[]
  selected: number
  bypassed: boolean
  showBandCurves: boolean
  showSpectrum: boolean
  /// Live handle on the analyser frame — see [`SpectrumLayer`] for why this is
  /// a ref rather than a value.
  spectrumRef: RefObject<SpectrumFrame | null>
  onSelect: (index: number) => void
  onBandChange: (index: number, patch: Partial<Band>) => void
}

export function ResponseGraph({
  bands,
  selected,
  bypassed,
  showBandCurves,
  showSpectrum,
  spectrumRef,
  onSelect,
  onBandChange,
}: ResponseGraphProps) {
  const svgRef = useRef<SVGSVGElement>(null)
  const dragging = useRef<number | null>(null)
  const [size, setSize] = useState<Size>({ width: 960, height: 420 })
  const [cursor, setCursor] = useState<{ x: number; y: number } | null>(null)
  const [dragged, setDragged] = useState<number | null>(null)

  useEffect(() => {
    const node = svgRef.current
    if (!node) return
    const measure = () => {
      const rect = node.getBoundingClientRect()
      if (rect.width <= 0 || rect.height <= 0) return
      setSize((current) =>
        Math.abs(current.width - rect.width) < 0.5 &&
        Math.abs(current.height - rect.height) < 0.5
          ? current
          : { width: rect.width, height: rect.height },
      )
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(node)
    return () => observer.disconnect()
  }, [])

  const { width, height } = size
  const sumPath = useMemo(
    () => sumCurvePath(bands, width, height),
    [bands, height, width],
  )
  const perBandPaths = useMemo(
    () =>
      showBandCurves
        ? bands.map((band, index) =>
            band.active && index !== selected
              ? bandCurvePath(band, width, height)
              : null,
          )
        : null,
    [bands, height, selected, showBandCurves, width],
  )
  const selectedPath = useMemo(() => {
    const band = bands[selected]
    return band?.active ? bandCurvePath(band, width, height) : null
  }, [bands, height, selected, width])

  /// Client coordinates mapped into the same viewBox space the curves, grid and
  /// nodes are drawn in, so a stale measurement can never make drawing and
  /// hit-testing disagree.
  const toGraphPoint = useCallback(
    (clientX: number, clientY: number) => {
      const rect = svgRef.current?.getBoundingClientRect()
      if (!rect || rect.width <= 0 || rect.height <= 0) return null
      return {
        x: ((clientX - rect.left) / rect.width) * width,
        y: ((clientY - rect.top) / rect.height) * height,
      }
    },
    [height, width],
  )

  const dragBand = useCallback(
    (index: number, clientX: number, clientY: number) => {
      const point = toGraphPoint(clientX, clientY)
      const band = bands[index]
      if (!point || !band) return
      const patch: Partial<Band> = {
        freq: clamp(xToFrequency(point.x, width), MIN_FREQ, MAX_FREQ),
      }
      if (bandHasGain(band.bandType)) {
        patch.gainDb = clamp(yToGain(point.y, height), -GAIN_RANGE, GAIN_RANGE)
      }
      onBandChange(index, patch)
    },
    [bands, height, onBandChange, toGraphPoint, width],
  )

  const onNodeWheel = (event: ReactWheelEvent<SVGGElement>, index: number) => {
    event.preventDefault()
    const band = bands[index]
    if (!band) return
    const scale = event.shiftKey ? 0.02 : 0.12
    onBandChange(index, {
      q: clamp(band.q - Math.sign(event.deltaY) * scale, MIN_Q, MAX_Q),
    })
  }

  const cursorFreq = cursor ? xToFrequency(cursor.x, width) : null
  const zeroY = gainToY(0, height)

  return (
    <div className="response-stack">
      <SpectrumLayer frameRef={spectrumRef} visible={showSpectrum && !bypassed} />
      <svg
      ref={svgRef}
      className={`response${bypassed ? ' is-bypassed' : ''}`}
      viewBox={`0 0 ${width} ${height}`}
      preserveAspectRatio="none"
      onPointerMove={(event: ReactPointerEvent<SVGSVGElement>) => {
        if (dragging.current !== null) {
          dragBand(dragging.current, event.clientX, event.clientY)
          setCursor(null)
          return
        }
        setCursor(toGraphPoint(event.clientX, event.clientY))
      }}
      onPointerLeave={() => setCursor(null)}
      onPointerUp={(event) => {
        dragging.current = null
        setDragged(null)
        if (event.currentTarget.hasPointerCapture(event.pointerId)) {
          event.currentTarget.releasePointerCapture(event.pointerId)
        }
      }}
      onPointerCancel={() => {
        dragging.current = null
        setDragged(null)
      }}
    >
      <defs>
        <linearGradient id="graph-shade" x1="0" x2="1" y1="0" y2="0">
          <stop offset="0" stopColor="#101a26" stopOpacity=".42" />
          <stop offset=".22" stopColor="#0c1118" stopOpacity=".08" />
          <stop offset=".72" stopColor="#0c1017" stopOpacity=".04" />
          <stop offset="1" stopColor="#151226" stopOpacity=".32" />
        </linearGradient>
        <linearGradient id="curve-fill" x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stopColor="var(--accent)" stopOpacity="0.26" />
          <stop offset="0.5" stopColor="var(--accent)" stopOpacity="0.05" />
          <stop offset="1" stopColor="var(--accent)" stopOpacity="0.18" />
        </linearGradient>
        <filter id="curve-glow" x="-4%" y="-30%" width="108%" height="160%">
          <feGaussianBlur stdDeviation="4" />
        </filter>
      </defs>

      <rect
        className="graph-background"
        width={width}
        height={height}
        fill="url(#graph-shade)"
      />

      <g className="grid">
        {GRID_FREQUENCIES.map((frequency) => {
          const x = frequencyToX(frequency, width)
          const labelled = LABELLED_FREQUENCIES.includes(frequency)
          return (
            <line
              key={frequency}
              x1={x}
              x2={x}
              y1={0}
              y2={height}
              className={labelled ? 'grid-line is-major' : 'grid-line'}
            />
          )
        })}
        {GRID_GAINS.map((gain) => (
          <line
            key={gain}
            x1={0}
            x2={width}
            y1={gainToY(gain, height)}
            y2={gainToY(gain, height)}
            className={gain === 0 ? 'grid-line is-zero' : 'grid-line'}
          />
        ))}
      </g>

      <g className="axis">
        {LABELLED_FREQUENCIES.map((frequency) => (
          <text
            key={frequency}
            x={frequencyToX(frequency, width)}
            y={height - 9}
            className="axis-freq"
          >
            {formatFrequency(frequency)}
          </text>
        ))}
        {GRID_GAINS.filter((gain) => gain !== 0).map((gain) => (
          <text
            key={gain}
            x={width - 7}
            y={gainToY(gain, height) - 5}
            className="axis-gain"
          >
            {gain > 0 ? `+${gain}` : gain}
          </text>
        ))}
      </g>

      {cursor && cursorFreq !== null && (
        <g className="cursor-guide">
          <line x1={cursor.x} x2={cursor.x} y1={0} y2={height} />
          <text x={width - 9} y={16}>
            {formatFrequency(cursorFreq)} Hz{' '}
            {formatGain(sumDbAt(bands, cursorFreq))} dB
          </text>
        </g>
      )}

      {perBandPaths?.map((path, index) =>
        path ? (
          <path
            key={index}
            d={path}
            className="band-curve"
            style={{ '--band': BAND_COLORS[index] } as CSSProperties}
          />
        ) : null,
      )}

      {selectedPath && (
        <path
          d={selectedPath}
          className="band-curve is-selected"
          style={{ '--band': BAND_COLORS[selected] } as CSSProperties}
        />
      )}

      <path
        className="curve-fill"
        d={`${sumPath} L${width},${zeroY} L0,${zeroY} Z`}
        fill="url(#curve-fill)"
      />
      <path d={sumPath} className="curve-glow" filter="url(#curve-glow)" />
      <path d={sumPath} className="curve-line" />

      {bands.map((band, index) => {
        const nodeGain = bandHasGain(band.bandType) ? band.gainDb : 0
        const x = frequencyToX(band.freq, width)
        const y = gainToY(nodeGain, height)
        const isSelected = selected === index
        const readoutX =
          clamp(x, READOUT_WIDTH / 2 + 6, Math.max(READOUT_WIDTH / 2 + 6, width - READOUT_WIDTH / 2 - 6)) - x
        const readoutY = y < READOUT_HEIGHT + 34 ? 40 : -40
        return (
          <g
            key={index}
            className={`node${isSelected ? ' is-selected' : ''}${band.active ? '' : ' is-off'}`}
            transform={`translate(${x} ${y})`}
            style={{ '--band': BAND_COLORS[index] } as CSSProperties}
            role="slider"
            tabIndex={0}
            aria-label={`Band ${index + 1} ${filterKind(band.bandType).label}`}
            aria-valuemin={MIN_FREQ}
            aria-valuemax={MAX_FREQ}
            aria-valuenow={Math.round(band.freq)}
            aria-valuetext={`${formatFrequency(band.freq)} hertz, ${formatGain(band.gainDb)} decibel`}
            onPointerDown={(event) => {
              onSelect(index)
              dragging.current = index
              setDragged(index)
              setCursor(null)
              svgRef.current?.setPointerCapture(event.pointerId)
            }}
            onDoubleClick={() => onBandChange(index, { gainDb: 0, q: 1 })}
            onWheel={(event) => onNodeWheel(event, index)}
            onKeyDown={(event) => {
              const factor = event.shiftKey ? 1.01 : 1.05
              if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
                event.preventDefault()
                onBandChange(index, {
                  freq: clamp(
                    band.freq * (event.key === 'ArrowRight' ? factor : 1 / factor),
                    MIN_FREQ,
                    MAX_FREQ,
                  ),
                })
              }
              if (event.key === 'ArrowUp' || event.key === 'ArrowDown') {
                event.preventDefault()
                if (!bandHasGain(band.bandType)) return
                onBandChange(index, {
                  gainDb: clamp(
                    band.gainDb + (event.key === 'ArrowUp' ? 0.5 : -0.5),
                    -GAIN_RANGE,
                    GAIN_RANGE,
                  ),
                })
              }
            }}
          >
            {dragged === index && (
              <g
                className="node-readout"
                transform={`translate(${readoutX} ${readoutY})`}
              >
                <rect
                  x={-READOUT_WIDTH / 2}
                  y={-READOUT_HEIGHT / 2}
                  width={READOUT_WIDTH}
                  height={READOUT_HEIGHT}
                  rx="4"
                />
                <text textAnchor="middle" y="4">
                  {formatFrequency(band.freq)} Hz · {formatGain(band.gainDb)} dB ·
                  Q {formatQ(band.q)}
                </text>
              </g>
            )}
            <circle className="node-halo" r={isSelected ? 19 : 15} />
            <circle className="node-ring" r={isSelected ? 11 : 9} />
            <text textAnchor="middle" dominantBaseline="central">
              {index + 1}
            </text>
          </g>
        )
      })}
      </svg>
    </div>
  )
}
