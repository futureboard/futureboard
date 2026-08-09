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
  MIN_FREQ,
  bandCurvePath,
  bandHasGain,
  clamp,
  filterKind,
  formatFrequency,
  formatGain,
  formatQ,
  frequencyToX,
  gainToY,
  scaleQ,
  sumCurveAreaPath,
  sumCurvePath,
  sumDbAt,
  xToFrequency,
  yToGain,
} from '../lib/eq'
import { pulseBandNode } from '../lib/motion'

const READOUT_WIDTH = 148
const READOUT_HEIGHT = 30

/// One wheel notch in `deltaY` units, matching a conventional mouse detent.
/// Trackpads and high-resolution wheels report smaller values and so produce
/// correspondingly smaller, smooth steps.
const WHEEL_NOTCH = 100

/// Q multiplier per wheel notch. 1.15 is roughly two notches per semitone-ish
/// step of bandwidth — brisk enough to cross the range in a short gesture, fine
/// enough to place a value without overshooting.
const Q_WHEEL_COARSE = 1.15
const Q_WHEEL_FINE = 1.03

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
  /// Band currently auditioned alone, or `SOLO_NONE`.
  soloBand: number
  /// Rate the host is running at. Not read directly — the curve helpers take it
  /// from module state — but listed as a memo dependency so a rate change
  /// redraws instead of leaving a curve computed for the previous rate.
  sampleRate: number
  onSelect: (index: number) => void
  onBandChange: (index: number, patch: Partial<Band>) => void
  onToggleSolo: (index: number) => void
  onSetSolo: (index: number) => void
}

export function ResponseGraph({
  bands,
  selected,
  bypassed,
  showBandCurves,
  showSpectrum,
  spectrumRef,
  soloBand,
  sampleRate,
  onSelect,
  onBandChange,
  onToggleSolo,
  onSetSolo,
}: ResponseGraphProps) {
  const svgRef = useRef<SVGSVGElement>(null)
  const dragging = useRef<number | null>(null)
  /// Solo state to put back when a momentary right-drag audition ends, or
  /// `null` when the gesture in progress is not an audition. `SOLO_NONE` is a
  /// meaningful value to restore, hence `null` rather than `-1` as the sentinel.
  const auditionRestore = useRef<number | null>(null)
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
  const nodeRefs = useRef<Array<SVGGElement | null>>([])

  useEffect(() => {
    pulseBandNode(nodeRefs.current[selected] ?? null)
  }, [selected])

  const sumPath = useMemo(
    () => sumCurvePath(bands, width, height, sampleRate),
    [bands, height, width, sampleRate],
  )
  const sumArea = useMemo(
    () => sumCurveAreaPath(bands, width, height, sampleRate),
    [bands, height, width, sampleRate],
  )
  const perBandPaths = useMemo(
    () =>
      showBandCurves
        ? bands.map((band, index) =>
            band.active && index !== selected
              ? bandCurvePath(band, width, height, sampleRate)
              : null,
          )
        : null,
    [bands, height, selected, showBandCurves, width, sampleRate],
  )
  const selectedPath = useMemo(() => {
    const band = bands[selected]
    return band?.active
      ? bandCurvePath(band, width, height, sampleRate)
      : null
  }, [bands, height, selected, width, sampleRate])
  /// Ghost of the fully-triggered dynamic gain (gain + range), Pro-Q style.
  const selectedDynamicExtentPath = useMemo(() => {
    const band = bands[selected]
    if (
      !band?.active ||
      !band.dynamic ||
      !bandHasGain(band.bandType) ||
      Math.abs(band.rangeDb) < 0.01
    ) {
      return null
    }
    return bandCurvePath(
      { ...band, gainDb: band.gainDb + band.rangeDb },
      width,
      height,
      sampleRate,
    )
  }, [bands, height, selected, width, sampleRate])

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
      // The audition drag moves the band exactly like a normal drag, both axes.
      // An earlier revision locked it to frequency on the theory that gain is
      // inaudible while soloed and would therefore be changed blindly — but the
      // point of the gesture is to place the band *while* listening, and half a
      // drag is not that. The node keeps tracking the cursor, so the gain being
      // dialled in stays visible on the curve even when it is not audible.
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
    // Ratio per notch rather than a fixed amount, so a notch is the same
    // perceived step at Q 0.3 as at Q 8. Notches are also accumulated by
    // magnitude instead of `Math.sign`, which threw away how far the wheel
    // actually moved and made every gesture exactly one step regardless.
    const notches = clamp(event.deltaY / WHEEL_NOTCH, -4, 4)
    const perNotch = event.shiftKey ? Q_WHEEL_FINE : Q_WHEEL_COARSE
    onBandChange(index, { q: scaleQ(band.q, Math.pow(perNotch, -notches)) })
  }

  /// End any drag, releasing a momentary audition if one was running.
  ///
  /// Every teardown path routes through here — pointer up, cancel and the
  /// window-level safety net below — because a missed release would leave the
  /// EQ silently stuck bandpassing one band.
  const endGesture = useCallback(() => {
    dragging.current = null
    setDragged(null)
    if (auditionRestore.current !== null) {
      onSetSolo(auditionRestore.current)
      auditionRestore.current = null
    }
  }, [onSetSolo])

  /// Drive the drag from the window rather than from the SVG's own handlers.
  ///
  /// The right-button gesture cannot rely on pointer capture or on the SVG
  /// staying the event target: opening a context menu makes Blink fire
  /// `pointercancel`, which drops capture and would strand a half-finished
  /// drag. Window listeners see the move regardless of capture, target, or
  /// whether the pointer wandered outside the graph, which is also what makes
  /// a release outside the window release the audition instead of latching it.
  useEffect(() => {
    const onWindowMove = (event: globalThis.PointerEvent) => {
      if (dragging.current === null) return
      dragBand(dragging.current, event.clientX, event.clientY)
      setCursor(null)
    }
    const onWindowUp = () => {
      if (dragging.current !== null || auditionRestore.current !== null) {
        endGesture()
      }
    }
    window.addEventListener('pointermove', onWindowMove)
    window.addEventListener('pointerup', onWindowUp)
    window.addEventListener('blur', onWindowUp)
    return () => {
      window.removeEventListener('pointermove', onWindowMove)
      window.removeEventListener('pointerup', onWindowUp)
      window.removeEventListener('blur', onWindowUp)
    }
  }, [dragBand, endGesture])

  const cursorFreq = cursor ? xToFrequency(cursor.x, width) : null

  return (
    <div className="response-stack">
      <SpectrumLayer frameRef={spectrumRef} visible={showSpectrum && !bypassed} />
      <svg
      ref={svgRef}
      className={`response${bypassed ? ' is-bypassed' : ''}`}
      viewBox={`0 0 ${width} ${height}`}
      preserveAspectRatio="none"
      onPointerMove={(event: ReactPointerEvent<SVGSVGElement>) => {
        // Dragging is handled window-side (see above); this only feeds the
        // hover readout, which is meaningless mid-drag.
        if (dragging.current !== null) return
        setCursor(toGraphPoint(event.clientX, event.clientY))
      }}
      onPointerLeave={() => setCursor(null)}
      // The audition gesture is driven by the right button, whose context menu
      // would otherwise pop up over the graph mid-drag.
      onContextMenu={(event) => event.preventDefault()}
      onPointerUp={(event) => {
        endGesture()
        if (event.currentTarget.hasPointerCapture(event.pointerId)) {
          event.currentTarget.releasePointerCapture(event.pointerId)
        }
      }}
      // Deliberately not wired to `endGesture`: Blink fires `pointercancel`
      // when it opens a context menu, and treating that as "gesture over"
      // would kill the right-button drag the instant it starts. The
      // window-level `pointerup` above is the real end of the gesture.
      onPointerCancel={() => setCursor(null)}
    >
      <defs>
        <linearGradient id="graph-shade" x1="0" x2="1" y1="0" y2="1">
          <stop offset="0" stopColor="#0b1520" stopOpacity=".55" />
          <stop offset=".35" stopColor="#070b10" stopOpacity=".12" />
          <stop offset=".7" stopColor="#080a0e" stopOpacity=".08" />
          <stop offset="1" stopColor="#140e12" stopOpacity=".35" />
        </linearGradient>
        <linearGradient id="curve-fill" x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stopColor="var(--color-signal)" stopOpacity="0.32" />
          <stop offset="0.48" stopColor="var(--color-signal)" stopOpacity="0.06" />
          <stop offset="1" stopColor="var(--color-signal)" stopOpacity="0.2" />
        </linearGradient>
        <filter id="curve-glow" x="-4%" y="-30%" width="108%" height="160%">
          <feGaussianBlur stdDeviation="3.5" />
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
            {formatGain(sumDbAt(bands, cursorFreq, sampleRate))} dB
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

      {selectedDynamicExtentPath && (
        <path
          d={selectedDynamicExtentPath}
          className="band-curve is-dynamic-extent"
          style={{ '--band': BAND_COLORS[selected] } as CSSProperties}
        />
      )}

      {selectedPath && (
        <path
          d={selectedPath}
          className="band-curve is-selected"
          style={{ '--band': BAND_COLORS[selected] } as CSSProperties}
        />
      )}

      <path className="curve-fill" d={sumArea} fill="url(#curve-fill)" />
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
            ref={(element) => {
              nodeRefs.current[index] = element
            }}
            className={`node${isSelected ? ' is-selected' : ''}${band.active ? '' : ' is-off'}${soloBand === index ? ' is-soloed' : ''}`}
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
              // Alt-click latches the audition on and off, for when you want to
              // keep listening with both hands free.
              if (event.altKey && event.button === 0) {
                event.preventDefault()
                onToggleSolo(index)
                return
              }
              // Right-button *hold* is the momentary audition: solo engages for
              // as long as the button is down and the band drags normally
              // underneath it, so you can sweep the frequency and hear the
              // range follow. Releasing restores whatever solo state was there
              // before, which is what makes it safe to reach for mid-mix.
              if (event.button === 2) {
                event.preventDefault()
                auditionRestore.current = soloBand
                onSetSolo(index)
              }
              dragging.current = index
              setDragged(index)
              setCursor(null)
              svgRef.current?.setPointerCapture(event.pointerId)
            }}
            // Also suppressed on the node itself, not just the SVG: the menu
            // must be cancelled before it can interrupt the drag it starts.
            onContextMenu={(event) => event.preventDefault()}
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
            <title>
              {`Band ${index + 1} — drag to move${band.active ? '' : ' (switches it on)'}` +
                ', wheel for Q, hold right-button to sweep and listen, ' +
                'Alt-click to keep listening'}
            </title>
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
