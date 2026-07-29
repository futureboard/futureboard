/**
 * Meter bay: IN ladder · enamel VU movement · OUT ladder.
 *
 * Host telemetry is consumed through a ref, so the faceplate never re-renders
 * at meter rate. One RAF loop advances the ballistics, paints both LED
 * canvases, rotates the needle by attribute, and writes the numeric readouts at
 * a fixed 15 Hz — no React state on the hot path.
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

/** 0 VU is referenced to −18 dBFS, the usual digital alignment. */
const OUTPUT_REF_DBFS = -18

const FACE_W = 260
const FACE_H = 140
const PIVOT_X = 130
const PIVOT_Y = 158
const NEEDLE_R = 124
const SCALE_R = 128
const LABEL_R = 110
const TICK_MAJOR = 10
const TICK_MINOR = 6
const READOUT_HZ = 15

/** Engraved legend beside the LED ladders (dBFS). */
const LADDER_LEGEND = [0, -6, -12, -20, -30, -45, -60]

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

  const gap = 1.4
  const segH = (cssH - gap * (LADDER_SEGMENTS - 1)) / LADDER_SEGMENTS
  const peakSeg = Math.round(peakNorm * LADDER_SEGMENTS)
  const rmsSeg = Math.round(rmsNorm * LADDER_SEGMENTS)
  const holdSeg = Math.round(hold * LADDER_SEGMENTS)

  for (let i = 0; i < LADDER_SEGMENTS; i++) {
    // i = 0 at the top (hot end), matching hardware LED strips.
    const fromTop = LADDER_SEGMENTS - i
    const y = i * (segH + gap)
    const lit = fromTop <= peakSeg
    const rmsLit = fromTop <= rmsSeg
    const isHold = fromTop === holdSeg && holdSeg > 0

    let fill = colors.idle
    if (lit) {
      const t = fromTop / LADDER_SEGMENTS
      if (t > 0.9) fill = colors.hot
      else if (t > 0.72) fill = colors.warm
      else fill = colors.cool
    }

    ctx.globalAlpha = lit ? (rmsLit ? 1 : 0.62) : 0.2
    ctx.fillStyle = fill
    ctx.beginPath()
    if (typeof ctx.roundRect === 'function') {
      ctx.roundRect(0, y, cssW, segH, 1.5)
    } else {
      ctx.rect(0, y, cssW, segH)
    }
    ctx.fill()

    if (isHold) {
      ctx.globalAlpha = 1
      ctx.fillStyle = colors.hold
      ctx.fillRect(0, y, cssW, Math.max(1.4, segH * 0.5))
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
      viewBox={`0 0 ${FACE_W} ${FACE_H}`}
      role="img"
      aria-label={mode === 'reduction' ? 'Gain reduction meter' : 'Output level meter'}
    >
      <defs>
        <linearGradient id={`${faceId}-face`} x1="0.2" x2="0.8" y1="0" y2="1">
          <stop offset="0" stopColor="var(--meter-face-hi)" />
          <stop offset="0.5" stopColor="var(--meter-face)" />
          <stop offset="1" stopColor="var(--meter-face-lo)" />
        </linearGradient>
        <radialGradient id={`${faceId}-vignette`} cx="0.5" cy="0.15" r="0.95">
          <stop offset="0" stopColor="rgba(255,255,255,0.16)" />
          <stop offset="0.65" stopColor="rgba(0,0,0,0)" />
          <stop offset="1" stopColor="rgba(0,0,0,0.28)" />
        </radialGradient>
        <linearGradient id={`${faceId}-glass`} x1="0" x2="1" y1="0" y2="1">
          <stop offset="0" stopColor="rgba(255,255,255,0.20)" />
          <stop offset="0.42" stopColor="rgba(255,255,255,0.05)" />
          <stop offset="0.43" stopColor="rgba(255,255,255,0.01)" />
          <stop offset="1" stopColor="rgba(0,0,0,0.14)" />
        </linearGradient>
        <clipPath id={`${faceId}-clip`}>
          <rect x="0" y="0" width={FACE_W} height={FACE_H} rx="4" />
        </clipPath>
        <filter id={`${faceId}-needle`} x="-40%" y="-40%" width="180%" height="180%">
          <feDropShadow dx="0.6" dy="1.6" stdDeviation="0.9" floodOpacity="0.4" />
        </filter>
      </defs>

      <g clipPath={`url(#${faceId}-clip)`}>
        <rect x="0" y="0" width={FACE_W} height={FACE_H} fill={`url(#${faceId}-face)`} />

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

        <text className="caption" x={FACE_W / 2} y="20">
          {mode === 'reduction' ? 'GAIN REDUCTION' : 'OUTPUT LEVEL'}
        </text>
        <text className="unit" x={FACE_W / 2} y="98">
          {mode === 'reduction' ? 'dB' : 'VU'}
        </text>
        <text className="brand" x="16" y="130" textAnchor="start">
          Z—COMP
        </text>
        <text className="brand" x={FACE_W - 16} y="130" textAnchor="end">
          {mode === 'reduction' ? 'VU BALLISTICS' : '0 VU = −18 dBFS'}
        </text>

        <g
          ref={needleRef}
          transform={`rotate(${restAngle.toFixed(3)} ${PIVOT_X} ${PIVOT_Y})`}
          filter={`url(#${faceId}-needle)`}
        >
          <polygon
            className="needle"
            points={`${PIVOT_X - 2.6},${PIVOT_Y} ${PIVOT_X - 0.7},${PIVOT_Y - NEEDLE_R} ${PIVOT_X + 0.7},${PIVOT_Y - NEEDLE_R} ${PIVOT_X + 2.6},${PIVOT_Y}`}
          />
        </g>

        <rect
          x="0"
          y="0"
          width={FACE_W}
          height={FACE_H}
          fill={`url(#${faceId}-vignette)`}
          pointerEvents="none"
        />
        <rect
          x="0"
          y="0"
          width={FACE_W}
          height={FACE_H * 0.52}
          fill={`url(#${faceId}-glass)`}
          pointerEvents="none"
        />
        <text ref={parkedRef} className="parked" x={FACE_W / 2} y="118" style={{ display: 'none' }}>
          no signal
        </text>
      </g>
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
    const inState: LadderState = { peak: 0, rms: 0, hold: createPeakHold() }
    const outState: LadderState = { peak: 0, rms: 0, hold: createPeakHold() }
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
      idle: 'rgba(255,255,255,0.05)',
      hold: 'rgba(245,240,230,0.95)',
    }

    const syncColors = () => {
      const root = rootRef.current
      if (!root) return
      colors.cool = readCssColor(root, '--led-cool', colors.cool)
      colors.warm = readCssColor(root, '--led-warm', colors.warm)
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

      const target = isLive
        ? normFor(
            meterMode,
            meterMode === 'reduction' ? grDb : linearToDb(outRmsLin) - OUTPUT_REF_DBFS,
          )
        : normFor(meterMode, meterMode === 'reduction' ? 0 : -20)

      needle = advance(needle, target, dt)
      needleRef.current?.setAttribute(
        'transform',
        `rotate(${angleFor(needle).toFixed(3)} ${PIVOT_X} ${PIVOT_Y})`,
      )

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
      inClipRef.current?.classList.toggle('is-on', inClip)
      outClipRef.current?.classList.toggle('is-on', outClip)

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

  // A skin change repaints the LED colours on the next RAF; drop the backing
  // stores so the canvases also pick up any new device pixel ratio.
  useEffect(() => {
    for (const canvas of [inCanvasRef.current, outCanvasRef.current]) {
      if (canvas) {
        canvas.width = 0
        canvas.height = 0
      }
    }
  }, [live, mode])

  return (
    <div ref={rootRef} className="meter-bay" aria-label="Level meters">
      <div className="movement">
        <div className={`glass${live ? '' : ' is-dark'}`}>
          <VuFace
            mode={mode}
            faceId={faceId}
            needleRef={needleRef}
            parkedRef={parkedRef}
          />
          <span className="screw tl" aria-hidden="true" />
          <span className="screw tr" aria-hidden="true" />
          <span className="screw bl" aria-hidden="true" />
          <span className="screw br" aria-hidden="true" />
          <button
            ref={clipBtnRef}
            type="button"
            className="clip-flag"
            hidden
            title="Output clipped — click to clear"
            onClick={onClearOutClip}
          >
            CLIP
          </button>
        </div>
        <div className="gr-strip">
          <span className="tag">Reduction</span>
          <output ref={grReadoutRef} aria-live="off">
            —
          </output>
          <small>dB</small>
        </div>
      </div>

      <div className="ladders">
        <div className="io">
          <span className="io-tag">In</span>
          <span ref={inClipRef} className="clip-led" aria-hidden="true" />
          <canvas ref={inCanvasRef} className="ladder" aria-hidden="true" />
          <output ref={inReadoutRef}>—</output>
        </div>

        <div className="io-scale" aria-hidden="true">
          {LADDER_LEGEND.map((db) => (
            <span key={db}>{db}</span>
          ))}
        </div>

        <div className="io">
          <span className="io-tag">Out</span>
          <span ref={outClipRef} className="clip-led" aria-hidden="true" />
          <canvas ref={outCanvasRef} className="ladder" aria-hidden="true" />
          <output ref={outReadoutRef}>—</output>
        </div>
      </div>
    </div>
  )
}
