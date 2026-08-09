import { useEffect, useRef, type RefObject } from 'react'
import type { MeterFrame } from './bridge'

type Props = {
  metersRef: RefObject<MeterFrame | null>
  live: boolean
}

const FLOOR_DB = -60
const REDUCTION_RANGE_DB = 24
/** Peak-hold dwell before the marker starts falling, in ms. */
const PEAK_HOLD_MS = 1_200

export function linearToDb(value: number) {
  return 20 * Math.log10(Math.max(value, 0.000_001))
}

export function levelNorm(value: number) {
  return Math.min(1, Math.max(0, (linearToDb(value) - FLOOR_DB) / -FLOOR_DB))
}

export function reductionNorm(value: number) {
  return Math.min(1, Math.max(0, value / REDUCTION_RANGE_DB))
}

/**
 * Host telemetry meters.
 *
 * Painted from a ref inside one rAF loop and written straight to style/text, so
 * 60 Hz metering never invalidates the React tree that owns the rack. Every
 * value here comes from the bridge's `futureboard.meters` frame; with no
 * connection the meters read empty rather than inventing motion.
 */
export function Meters({ metersRef, live }: Props) {
  const inFill = useRef<HTMLDivElement>(null)
  const outFill = useRef<HTMLDivElement>(null)
  const grFill = useRef<HTMLDivElement>(null)
  const inPeak = useRef<HTMLDivElement>(null)
  const outPeak = useRef<HTMLDivElement>(null)
  const inText = useRef<HTMLOutputElement>(null)
  const outText = useRef<HTMLOutputElement>(null)
  const grText = useRef<HTMLOutputElement>(null)
  const inClip = useRef<HTMLButtonElement>(null)
  const outClip = useRef<HTMLButtonElement>(null)
  const flowIn = useRef<HTMLDivElement>(null)
  const flowOut = useRef<HTMLDivElement>(null)
  const liveRef = useRef(live)
  liveRef.current = live

  useEffect(() => {
    let raf = 0
    let lastText = 0
    const hold = { in: 0, out: 0, inUntil: 0, outUntil: 0 }

    const paint = (now: number) => {
      const frame = liveRef.current ? metersRef.current : null
      const input = frame ? levelNorm(frame.inPeak) : 0
      const output = frame ? levelNorm(frame.outPeak) : 0
      const reduction = frame ? reductionNorm(frame.gainReductionDb) : 0

      if (inFill.current) inFill.current.style.transform = `scaleY(${input})`
      if (outFill.current) outFill.current.style.transform = `scaleY(${output})`
      if (grFill.current) grFill.current.style.transform = `scaleY(${reduction})`
      // Horizontal signal-flow pair — same frame, read left to right.
      if (flowIn.current) flowIn.current.style.transform = `scaleX(${input})`
      if (flowOut.current) flowOut.current.style.transform = `scaleX(${output})`

      if (input >= hold.in || now > hold.inUntil) {
        hold.in = input
        hold.inUntil = now + PEAK_HOLD_MS
      }
      if (output >= hold.out || now > hold.outUntil) {
        hold.out = output
        hold.outUntil = now + PEAK_HOLD_MS
      }
      if (inPeak.current) inPeak.current.style.bottom = `${hold.in * 100}%`
      if (outPeak.current) outPeak.current.style.bottom = `${hold.out * 100}%`

      inClip.current?.classList.toggle('is-clipping', Boolean(frame?.inClip))
      outClip.current?.classList.toggle('is-clipping', Boolean(frame?.outClip))

      // Text is the expensive part; 15 Hz stays readable and legible.
      if (now - lastText > 66) {
        lastText = now
        if (inText.current) {
          inText.current.textContent = frame ? linearToDb(frame.inPeak).toFixed(1) : '—'
        }
        if (outText.current) {
          outText.current.textContent = frame ? linearToDb(frame.outPeak).toFixed(1) : '—'
        }
        if (grText.current) {
          grText.current.textContent = frame ? `-${frame.gainReductionDb.toFixed(1)}` : '—'
        }
      }
      raf = requestAnimationFrame(paint)
    }

    raf = requestAnimationFrame(paint)
    return () => cancelAnimationFrame(raf)
  }, [metersRef])

  return (
    <section
      className="flex flex-col items-stretch gap-2.5"
      aria-label="Host telemetry meters"
    >
      <div className="flex items-end justify-center gap-7">
        <Meter
          label="Input"
          fillRef={inFill}
          peakRef={inPeak}
          textRef={inText}
          clipRef={inClip}
        />
        <Meter label="Reduction" fillRef={grFill} textRef={grText} reduction />
        <Meter
          label="Output"
          fillRef={outFill}
          peakRef={outPeak}
          textRef={outText}
          clipRef={outClip}
        />
      </div>
      <SignalFlow inRef={flowIn} outRef={flowOut} />
    </section>
  )
}

/**
 * A rack row's own IN→OUT meter.
 *
 * Fed by the per-stage telemetry MixStation publishes for each rack position:
 * `slotInPeak[slot]` is the level arriving at this stage and `slotOutPeak[slot]`
 * the level leaving it, after that module's output trim. When the native build
 * sends no per-stage array the bars stay dark rather than mirroring the master.
 */
export function StageMeter({
  metersRef,
  slot,
  live,
  accent,
}: {
  metersRef: RefObject<MeterFrame | null>
  /** Rack position, zero-based, in chain order. */
  slot: number
  live: boolean
  accent: string
}) {
  const inRef = useRef<HTMLDivElement>(null)
  const outRef = useRef<HTMLDivElement>(null)
  const liveRef = useRef(live)
  liveRef.current = live

  useEffect(() => {
    let raf = 0
    const paint = () => {
      raf = requestAnimationFrame(paint)
      const frame = liveRef.current ? metersRef.current : null
      const input = frame?.slotInPeak[slot]
      const output = frame?.slotOutPeak[slot]
      if (inRef.current) {
        inRef.current.style.transform = `scaleX(${input === undefined ? 0 : levelNorm(input)})`
      }
      if (outRef.current) {
        outRef.current.style.transform = `scaleX(${output === undefined ? 0 : levelNorm(output)})`
      }
    }
    raf = requestAnimationFrame(paint)
    return () => cancelAnimationFrame(raf)
  }, [metersRef, slot])

  return (
    <div className="flex shrink-0 items-center gap-2" aria-hidden>
      <span className="label-cap">In</span>
      <div className="flex w-24 flex-col gap-[3px]">
        <StageBar fillRef={inRef} accent={accent} />
        <StageBar fillRef={outRef} accent={accent} />
      </div>
      <span className="label-cap">Out</span>
    </div>
  )
}

function StageBar({
  fillRef,
  accent,
}: {
  fillRef: RefObject<HTMLDivElement | null>
  accent: string
}) {
  return (
    <div className="h-[3px] w-full overflow-hidden rounded-full bg-white/8">
      <div
        ref={fillRef}
        className="h-full w-full origin-left"
        style={{
          background: `linear-gradient(90deg, color-mix(in srgb, ${accent} 55%, transparent), ${accent})`,
          transform: 'scaleX(0)',
        }}
      />
    </div>
  )
}

/**
 * Horizontal in→out pair under the columns, restating the same frame as a
 * left-to-right signal flow. Both bars are master telemetry: this is the whole
 * chain's in and out, not a per-module
 * reading.
 */
function SignalFlow({
  inRef,
  outRef,
}: {
  inRef: RefObject<HTMLDivElement | null>
  outRef: RefObject<HTMLDivElement | null>
}) {
  return (
    <div className="flex items-center gap-2" aria-hidden>
      <span className="label-cap">In</span>
      <div className="flex min-w-40 flex-1 flex-col gap-[3px]">
        <FlowBar fillRef={inRef} />
        <FlowBar fillRef={outRef} />
      </div>
      <span className="label-cap">Out</span>
    </div>
  )
}

function FlowBar({ fillRef }: { fillRef: RefObject<HTMLDivElement | null> }) {
  return (
    <div className="h-[3px] w-full overflow-hidden rounded-full bg-white/6">
      <div
        ref={fillRef}
        className="h-full w-full origin-left bg-linear-to-r from-signal/55 via-signal to-danger"
        style={{ transform: 'scaleX(0)' }}
      />
    </div>
  )
}

function Meter({
  label,
  fillRef,
  peakRef,
  textRef,
  clipRef,
  reduction = false,
}: {
  label: string
  fillRef: RefObject<HTMLDivElement | null>
  peakRef?: RefObject<HTMLDivElement | null>
  textRef: RefObject<HTMLOutputElement | null>
  clipRef?: RefObject<HTMLButtonElement | null>
  reduction?: boolean
}) {
  return (
    <div className="flex flex-col items-center gap-1.5">
      <span className="label-cap">{label}</span>
      <div className="flex items-end gap-2">
        <div className="flex items-baseline gap-1">
          <output ref={textRef} className="readout w-[46px] text-right text-[15px] font-semibold">
            —
          </output>
          {/* Units stay visually quieter than the value they qualify. */}
          <span className="text-[10px] font-semibold text-ink-dim">dB</span>
        </div>
        <div
          className="relative h-11 w-2.5 overflow-hidden rounded-sm bg-white/6"
          aria-hidden
        >
          <div
            ref={fillRef}
            className={`absolute inset-x-0 bottom-0 h-full origin-bottom ${
              reduction
                ? 'top-0 bottom-auto origin-top bg-warn'
                : 'bg-linear-to-t from-signal/55 via-signal to-danger'
            }`}
            style={{ transform: 'scaleY(0)' }}
          />
          {peakRef ? (
            <div
              ref={peakRef}
              className="absolute inset-x-0 h-px bg-ink"
              style={{ bottom: '0%' }}
            />
          ) : null}
        </div>
        {clipRef ? (
          <button
            ref={clipRef}
            type="button"
            aria-label={`${label} clip indicator — click to clear`}
            title={`${label} clip — click to clear`}
            onClick={(event) => event.currentTarget.classList.remove('is-clipping')}
            className="clip-dot h-2 w-2 shrink-0 cursor-pointer self-start rounded-full border border-hairline-hi bg-transparent transition-colors duration-150 [&.is-clipping]:border-danger [&.is-clipping]:bg-danger"
          />
        ) : (
          <span className="w-2 shrink-0" aria-hidden />
        )}
      </div>
    </div>
  )
}
