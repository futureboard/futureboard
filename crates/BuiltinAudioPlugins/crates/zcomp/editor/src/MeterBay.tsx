/**
 * Hardware meter bay: IN ladder · enamel VU · OUT ladder.
 *
 * Host telemetry is consumed via a ref so App knobs never re-render at meter
 * rate. One RAF loop advances ballistics, paints both LED canvases, and rotates
 * the needle with a transform — no React setState on the hot path.
 */

import {
  useEffect,
  useId,
  useRef,
  type MutableRefObject,
  type RefObject,
} from 'react'
import type { MeterFrame } from './bridge'
import {
  LADDER_SEGMENTS,
  LED_ATTACK_TAU,
  LED_RELEASE_TAU,
  PEAK_HOLD_SECONDS,
  advance,
  angleFor,
  createPeakHold,
  formatLadderDb,
  holdNorm,
  ladderNormFromLinear,
  linearToDb,
  normFor,
  pushPeakHold,
  ticksFor,
  type MeterMode,
  type PeakHold,
} from './meter'

const OUTPUT_REF_DBFS = -18
const PIVOT_X = 100
const PIVOT_Y = 134
const NEEDLE_R = 100
const SCALE_R = 104
const LABEL_R = 86
const TICK_MAJOR = 9
const TICK_MINOR = 5
const READOUT_HZ = 15

type Props = {
  metersRef: MutableRefObject<MeterFrame | null>
  live: boolean
  mode: MeterMode
  onClearOutClip: () => void
}

type LadderState = {
  peak: number
  rms: number
  hold: PeakHold
}

function polar(norm: number, radius: number) {
  const rad = (angleFor(norm) * Math.PI) / 180
  return {
    x: PIVOT_X + Math.sin(rad) * radius,
    y: PIVOT_Y - Math.cos(rad) * radius,
  }
}

function arcPath(a: { x: number; y: number }, b: { x: number; y: number }) {
  return `M ${a.x.toFixed(2)} ${a.y.toFixed(2)} A ${SCALE_R} ${SCALE_R} 0 0 1 ${b.x.toFixed(2)} ${b.y.toFixed(2)}`
}

function readCssColor(el: Element, name: string, fallback: string) {
  const value = getComputedStyle(el).getPropertyValue(name).trim()
  return value || fallback
}

function paintLadder(
  canvas: HTMLCanvasElement,
  peakNorm: number,
  rmsNorm: number,
  hold: number,
  colors: { cool: string; warm: string; hot: string; idle: string; hold: string },
) {
  const dpr = Math.min(window.devicePixelRatio || 1, 2)
  const cssW = canvas.clientWidth || 12
  const cssH = canvas.clientHeight || 120
  const w = Math.max(1, Math.round(cssW * dpr))
  const h = Math.max(1, Math.round(cssH * dpr))
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w
    canvas.height = h
  }

  const ctx = canvas.getContext('2d')
  if (!ctx) return
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
  ctx.clearRect(0, 0, cssW, cssH)

  const gap = 1.15
  const segH = (cssH - gap * (LADDER_SEGMENTS - 1)) / LADDER_SEGMENTS
  const peakSeg = Math.round(peakNorm * LADDER_SEGMENTS)
  const rmsSeg = Math.round(rmsNorm * LADDER_SEGMENTS)
  const holdSeg = Math.round(hold * LADDER_SEGMENTS)

  for (let i = 0; i < LADDER_SEGMENTS; i++) {
    // i = 0 at top (hot), matching hardware LED strips.
    const fromTop = LADDER_SEGMENTS - i
    const y = i * (segH + gap)
    const lit = fromTop <= peakSeg
    const rmsLit = fromTop <= rmsSeg
    const isHold = fromTop === holdSeg && holdSeg > 0

    let fill = colors.idle
    if (lit) {
      const t = fromTop / LADDER_SEGMENTS
      if (t > 0.88) fill = colors.hot
      else if (t > 0.68) fill = colors.warm
      else fill = colors.cool
    }

    ctx.globalAlpha = lit ? (rmsLit ? 1 : 0.72) : 0.22
    ctx.fillStyle = fill
    const inset = lit ? 0 : 0.5
    const x = inset
    const ww = cssW - inset * 2
    if (typeof ctx.roundRect === 'function') {
      ctx.beginPath()
      ctx.roundRect(x, y, ww, segH, 1)
      ctx.fill()
    } else {
      ctx.fillRect(x, y, ww, segH)
    }

    if (isHold) {
      ctx.globalAlpha = 1
      ctx.fillStyle = colors.hold
      ctx.fillRect(0, y, cssW, Math.max(1.2, segH * 0.55))
    }
  }
  ctx.globalAlpha = 1
}

function VuFace({
  mode,
  faceId,
  needleRef,
  parkedRef,
}: {
  mode: MeterMode
  faceId: string
  needleRef: RefObject<SVGGElement | null>
  parkedRef: RefObject<SVGTextElement | null>
}) {
  const ticks = ticksFor(mode)
  const arcStart = polar(0, SCALE_R)
  const arcEnd = polar(1, SCALE_R)
  const hot = ticks.filter((tick) => tick.hot)
  const hotA = hot.length > 1 ? polar(Math.min(...hot.map((t) => t.norm)), SCALE_R) : null
  const hotB = hot.length > 1 ? polar(Math.max(...hot.map((t) => t.norm)), SCALE_R) : null
  const restAngle = angleFor(normFor(mode, mode === 'reduction' ? 0 : -20))

  return (
    <svg
      viewBox="0 0 200 120"
      role="img"
      aria-label={mode === 'reduction' ? 'Gain reduction meter' : 'Output level meter'}
    >
      <defs>
        <linearGradient id={`${faceId}-face`} x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stopColor="var(--meter-face-hi)" />
          <stop offset="0.55" stopColor="var(--meter-face)" />
          <stop offset="1" stopColor="var(--meter-face-lo)" />
        </linearGradient>
        <linearGradient id={`${faceId}-glass`} x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stopColor="rgba(255,255,255,0.22)" />
          <stop offset="0.35" stopColor="rgba(255,255,255,0.05)" />
          <stop offset="1" stopColor="rgba(0,0,0,0.12)" />
        </linearGradient>
        <filter id={`${faceId}-needle`} x="-40%" y="-40%" width="180%" height="180%">
          <feDropShadow dx="0.4" dy="1.1" stdDeviation="0.7" floodOpacity="0.45" />
        </filter>
      </defs>

      <rect x="0" y="0" width="200" height="120" rx="7" fill={`url(#${faceId}-face)`} />
      <path className="arc" d={arcPath(arcStart, arcEnd)} />
      {hotA && hotB ? <path className="arc hot" d={arcPath(hotA, hotB)} /> : null}

      {ticks.map((tick) => {
        const outer = polar(tick.norm, SCALE_R)
        const inner = polar(tick.norm, SCALE_R - (tick.major ? TICK_MAJOR : TICK_MINOR))
        const labelAt = polar(tick.norm, LABEL_R)
        return (
          <g key={`${mode}-${tick.norm}-${tick.label}`}>
            <line
              className={`tick${tick.major ? ' major' : ''}${tick.hot ? ' hot' : ''}`}
              x1={inner.x}
              y1={inner.y}
              x2={outer.x}
              y2={outer.y}
            />
            {tick.label ? (
              <text
                className={`label${tick.hot ? ' hot' : ''}`}
                x={labelAt.x}
                y={labelAt.y}
              >
                {tick.label}
              </text>
            ) : null}
          </g>
        )
      })}

      <text className="caption" x="100" y="18">
        {mode === 'reduction' ? 'GAIN REDUCTION' : 'OUTPUT'}
      </text>
      <text className="unit" x="100" y="88">
        {mode === 'reduction' ? 'dB' : 'VU'}
      </text>

      <g
        ref={needleRef}
        className="needle-g"
        transform={`rotate(${restAngle.toFixed(3)} ${PIVOT_X} ${PIVOT_Y})`}
        filter={`url(#${faceId}-needle)`}
      >
        <line
          className="needle"
          x1={PIVOT_X}
          y1={PIVOT_Y}
          x2={PIVOT_X}
          y2={PIVOT_Y - NEEDLE_R}
        />
        <circle className="hub" cx={PIVOT_X} cy={PIVOT_Y} r="4.2" />
      </g>

      <rect
        className="bezel"
        x="0.7"
        y="0.7"
        width="198.6"
        height="118.6"
        rx="6.4"
      />
      <rect
        x="2"
        y="2"
        width="196"
        height="52"
        rx="5"
        fill={`url(#${faceId}-glass)`}
        pointerEvents="none"
      />
      <text ref={parkedRef} className="parked" x="100" y="108" style={{ display: 'none' }}>
        no signal
      </text>
    </svg>
  )
}

export function MeterBay({ metersRef, live, mode, onClearOutClip }: Props) {
  const faceId = useId().replace(/:/g, '')
  const rootRef = useRef<HTMLDivElement>(null)
  const inCanvasRef = useRef<HTMLCanvasElement>(null)
  const outCanvasRef = useRef<HTMLCanvasElement>(null)
  const inReadoutRef = useRef<HTMLOutputElement>(null)
  const outReadoutRef = useRef<HTMLOutputElement>(null)
  const grReadoutRef = useRef<HTMLOutputElement>(null)
  const needleRef = useRef<SVGGElement>(null)
  const parkedRef = useRef<SVGTextElement>(null)
  const clipBtnRef = useRef<HTMLButtonElement>(null)
  const inClipRef = useRef<HTMLSpanElement>(null)
  const outClipRef = useRef<HTMLSpanElement>(null)
  const modeRef = useRef(mode)
  const liveRef = useRef(live)
  modeRef.current = mode
  liveRef.current = live

  useEffect(() => {
    const inState: LadderState = {
      peak: 0,
      rms: 0,
      hold: createPeakHold(),
    }
    const outState: LadderState = {
      peak: 0,
      rms: 0,
      hold: createPeakHold(),
    }
    let needle = normFor(modeRef.current, modeRef.current === 'reduction' ? 0 : -20)
    let last = performance.now()
    let readoutAcc = 0
    let lastGrText = ''
    let lastInText = ''
    let lastOutText = ''
    let raf = 0

    const colors = {
      cool: '#4a9a8c',
      warm: '#c9a35a',
      hot: '#c43a28',
      idle: 'rgba(255,255,255,0.06)',
      hold: 'rgba(245,240,230,0.95)',
    }

    const syncColors = () => {
      const root = rootRef.current
      if (!root) return
      colors.cool = readCssColor(root, '--btn-on', colors.cool)
      colors.hot = readCssColor(root, '--clip', colors.hot)
    }
    syncColors()

    const step = (now: number) => {
      const dt = Math.min((now - last) / 1000, 0.25)
      last = now
      readoutAcc += dt

      const frame = metersRef.current
      const isLive = liveRef.current && frame !== null
      const meterMode = modeRef.current

      const inPeakLin = isLive ? frame!.inPeak : 0
      const inRmsLin = isLive ? frame!.inRms : 0
      const outPeakLin = isLive ? frame!.outPeak : 0
      const outRmsLin = isLive ? frame!.outRms : 0
      const grDb = isLive ? frame!.gainReductionDb : 0
      const outClip = isLive ? frame!.outClip : false
      const inClip = isLive ? frame!.inClip : false

      inState.peak = advance(
        inState.peak,
        ladderNormFromLinear(inPeakLin),
        dt,
        LED_ATTACK_TAU,
        LED_RELEASE_TAU,
      )
      inState.rms = advance(
        inState.rms,
        ladderNormFromLinear(inRmsLin),
        dt,
        LED_RELEASE_TAU,
        LED_RELEASE_TAU * 1.4,
      )
      outState.peak = advance(
        outState.peak,
        ladderNormFromLinear(outPeakLin),
        dt,
        LED_ATTACK_TAU,
        LED_RELEASE_TAU,
      )
      outState.rms = advance(
        outState.rms,
        ladderNormFromLinear(outRmsLin),
        dt,
        LED_RELEASE_TAU,
        LED_RELEASE_TAU * 1.4,
      )

      if (isLive) {
        pushPeakHold(inState.hold, linearToDb(inPeakLin), dt)
        pushPeakHold(outState.hold, linearToDb(outPeakLin), dt)
      } else {
        inState.hold.db = -60
        inState.hold.age = PEAK_HOLD_SECONDS
        outState.hold.db = -60
        outState.hold.age = PEAK_HOLD_SECONDS
      }

      const target =
        isLive
          ? normFor(
              meterMode,
              meterMode === 'reduction'
                ? grDb
                : linearToDb(outRmsLin) - OUTPUT_REF_DBFS,
            )
          : normFor(meterMode, meterMode === 'reduction' ? 0 : -20)

      needle = advance(needle, target, dt)
      const angle = angleFor(needle)
      if (needleRef.current) {
        needleRef.current.setAttribute(
          'transform',
          `rotate(${angle.toFixed(3)} ${PIVOT_X} ${PIVOT_Y})`,
        )
      }

      if (inCanvasRef.current) {
        paintLadder(
          inCanvasRef.current,
          inState.peak,
          inState.rms,
          holdNorm(inState.hold),
          colors,
        )
      }
      if (outCanvasRef.current) {
        paintLadder(
          outCanvasRef.current,
          outState.peak,
          outState.rms,
          holdNorm(outState.hold),
          colors,
        )
      }

      if (parkedRef.current) {
        parkedRef.current.style.display = isLive ? 'none' : ''
      }
      if (clipBtnRef.current) {
        clipBtnRef.current.hidden = !outClip
      }
      if (inClipRef.current) {
        inClipRef.current.classList.toggle('is-on', inClip)
      }
      if (outClipRef.current) {
        outClipRef.current.classList.toggle('is-on', outClip)
      }

      if (readoutAcc >= 1 / READOUT_HZ) {
        readoutAcc = 0
        syncColors()
        const grText = isLive ? grDb.toFixed(1) : '—'
        if (grReadoutRef.current && grText !== lastGrText) {
          grReadoutRef.current.textContent = grText
          lastGrText = grText
        }
        const inText = isLive ? formatLadderDb(inPeakLin) : '—'
        if (inReadoutRef.current && inText !== lastInText) {
          inReadoutRef.current.textContent = inText
          lastInText = inText
        }
        const outText = isLive ? formatLadderDb(outPeakLin) : '—'
        if (outReadoutRef.current && outText !== lastOutText) {
          outReadoutRef.current.textContent = outText
          lastOutText = outText
        }
      }

      raf = requestAnimationFrame(step)
    }

    raf = requestAnimationFrame(step)
    const onVis = () => {
      if (document.visibilityState === 'visible') syncColors()
    }
    document.addEventListener('visibilitychange', onVis)
    return () => {
      cancelAnimationFrame(raf)
      document.removeEventListener('visibilitychange', onVis)
    }
  }, [metersRef])

  // Resync needle rest + cool LED color when the model skin changes.
  useEffect(() => {
    const root = rootRef.current
    if (!root) return
    // Force a color pull on next paint by toggling a data attr the loop reads
    // via getComputedStyle when visibility fires; also poke canvases size.
    for (const canvas of [inCanvasRef.current, outCanvasRef.current]) {
      if (canvas) {
        canvas.width = 0
        canvas.height = 0
      }
    }
  }, [live, mode])

  return (
    <div
      ref={rootRef}
      className={`meter-bay${!live ? ' is-dark' : ''}`}
      aria-label="Level meters"
    >
      <div className="io-strip">
        <span className="io-label">In</span>
        <span ref={inClipRef} className="clip-led" aria-hidden="true" />
        <canvas ref={inCanvasRef} className="io-ladder" aria-hidden="true" />
        <output ref={inReadoutRef}>—</output>
      </div>

      <div className="meter-well">
        <div className="meter">
          <VuFace
            mode={mode}
            faceId={faceId}
            needleRef={needleRef}
            parkedRef={parkedRef}
          />
          <button
            ref={clipBtnRef}
            type="button"
            className="clip-flag"
            hidden
            onClick={onClearOutClip}
          >
            CLIP
          </button>
        </div>
        <div className="gr-readout" aria-live="polite">
          <span>GR</span>
          <output ref={grReadoutRef}>—</output>
          <small>dB</small>
        </div>
      </div>

      <div className="io-strip">
        <span className="io-label">Out</span>
        <span ref={outClipRef} className="clip-led" aria-hidden="true" />
        <canvas ref={outCanvasRef} className="io-ladder" aria-hidden="true" />
        <output ref={outReadoutRef}>—</output>
      </div>
    </div>
  )
}
