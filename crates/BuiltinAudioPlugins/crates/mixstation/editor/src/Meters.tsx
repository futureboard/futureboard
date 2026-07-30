import { useEffect, useRef, type MutableRefObject } from 'react'
import type { MeterFrame } from './bridge'

type Props = {
  metersRef: MutableRefObject<MeterFrame | null>
  live: boolean
}

const FLOOR_DB = -60

export function linearToDb(value: number) {
  return 20 * Math.log10(Math.max(value, 0.000_001))
}

export function levelNorm(value: number) {
  return Math.min(1, Math.max(0, (linearToDb(value) - FLOOR_DB) / -FLOOR_DB))
}

export function reductionNorm(value: number) {
  return Math.min(1, Math.max(0, value / 24))
}

export function Meters({ metersRef, live }: Props) {
  const inputRef = useRef<HTMLDivElement>(null)
  const outputRef = useRef<HTMLDivElement>(null)
  const reductionRef = useRef<HTMLDivElement>(null)
  const inputTextRef = useRef<HTMLOutputElement>(null)
  const outputTextRef = useRef<HTMLOutputElement>(null)
  const reductionTextRef = useRef<HTMLOutputElement>(null)
  const inputClipRef = useRef<HTMLSpanElement>(null)
  const outputClipRef = useRef<HTMLSpanElement>(null)
  const liveRef = useRef(live)
  liveRef.current = live

  useEffect(() => {
    let raf = 0
    let lastText = 0
    const paint = (now: number) => {
      const frame = liveRef.current ? metersRef.current : null
      const input = frame ? levelNorm(frame.inPeak) : 0
      const output = frame ? levelNorm(frame.outPeak) : 0
      const reduction = frame ? reductionNorm(frame.gainReductionDb) : 0
      if (inputRef.current) inputRef.current.style.transform = `scaleY(${input})`
      if (outputRef.current) outputRef.current.style.transform = `scaleY(${output})`
      if (reductionRef.current) {
        reductionRef.current.style.transform = `scaleY(${reduction})`
      }
      inputClipRef.current?.classList.toggle('active', Boolean(frame?.inClip))
      outputClipRef.current?.classList.toggle('active', Boolean(frame?.outClip))

      if (now - lastText > 66) {
        lastText = now
        if (inputTextRef.current) {
          inputTextRef.current.textContent = frame
            ? linearToDb(frame.inPeak).toFixed(1)
            : '—'
        }
        if (outputTextRef.current) {
          outputTextRef.current.textContent = frame
            ? linearToDb(frame.outPeak).toFixed(1)
            : '—'
        }
        if (reductionTextRef.current) {
          reductionTextRef.current.textContent = frame
            ? frame.gainReductionDb.toFixed(1)
            : '—'
        }
      }
      raf = requestAnimationFrame(paint)
    }
    raf = requestAnimationFrame(paint)
    return () => cancelAnimationFrame(raf)
  }, [metersRef])

  return (
    <section className="meters" aria-label="Host telemetry meters">
      <Meter
        label="Input"
        fillRef={inputRef}
        outputRef={inputTextRef}
        clipRef={inputClipRef}
      />
      <Meter
        label="Reduction"
        fillRef={reductionRef}
        outputRef={reductionTextRef}
        reduction
      />
      <Meter
        label="Output"
        fillRef={outputRef}
        outputRef={outputTextRef}
        clipRef={outputClipRef}
      />
    </section>
  )
}

function Meter({
  label,
  fillRef,
  outputRef,
  clipRef,
  reduction = false,
}: {
  label: string
  fillRef: React.RefObject<HTMLDivElement | null>
  outputRef: React.RefObject<HTMLOutputElement | null>
  clipRef?: React.RefObject<HTMLSpanElement | null>
  reduction?: boolean
}) {
  return (
    <div className="meter">
      <span className="meter-label">{label}</span>
      <div className="meter-slot" aria-hidden="true">
        <div
          ref={fillRef}
          className={`meter-fill${reduction ? ' reduction' : ''}`}
        />
        {clipRef ? <span ref={clipRef} className="clip-dot" /> : null}
      </div>
      <output ref={outputRef}>—</output>
      <small>dB</small>
    </div>
  )
}
