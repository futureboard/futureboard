/**
 * Transfer-curve window: the static curve the selected circuit is running,
 * plus a live operating point.
 *
 * The curve comes from `device.circuitCurve` / `device.targetGrDb`, which
 * mirror `zcomp::dsp::model_coeffs` and `target_gr_db`. The dot comes from host
 * telemetry — nothing here invents a level. Both use the same
 * `toX`/`toY` transform, so the drawn curve and the plotted point cannot
 * disagree about where a decibel is.
 */

import { useEffect, useMemo, useRef, type MutableRefObject } from 'react'
import type { MeterFrame, ZcompParams } from './bridge'
import { circuitCurve, targetGrDb } from './device'
import { clamp, linearToDb } from './meter'

const VIEW_W = 208
const VIEW_H = 156
const PAD_L = 20
const PAD_R = 8
const PAD_T = 16
const PAD_B = 18
const MIN_DB = -60
const MAX_DB = 0
const GRID_DB = [-60, -48, -36, -24, -12, 0]

const PLOT_W = VIEW_W - PAD_L - PAD_R
const PLOT_H = VIEW_H - PAD_T - PAD_B

function toX(db: number) {
  return PAD_L + ((clamp(db, MIN_DB, MAX_DB) - MIN_DB) / (MAX_DB - MIN_DB)) * PLOT_W
}

function toY(db: number) {
  return PAD_T + (1 - (clamp(db, MIN_DB, MAX_DB) - MIN_DB) / (MAX_DB - MIN_DB)) * PLOT_H
}

/**
 * Steady-state output for one input level, including makeup and the dry/wet
 * blend. Mix is summed in the linear domain because that is where the DSP
 * blends it — a dB-domain approximation would draw parallel compression wrong.
 */
function outputDb(params: ZcompParams, inputDb: number) {
  const curve = circuitCurve(params)
  const gr = targetGrDb(curve, inputDb)
  const dry = Math.pow(10, inputDb / 20)
  const wet = dry * Math.pow(10, (params.makeupDb - gr) / 20)
  const amount = clamp(params.mix, 0, 100) / 100
  return linearToDb(dry * (1 - amount) + wet * amount)
}

type Props = {
  params: ZcompParams
  metersRef: MutableRefObject<MeterFrame | null>
  live: boolean
}

export function CurveDisplay({ params, metersRef, live }: Props) {
  const dotRef = useRef<SVGCircleElement>(null)
  const traceRef = useRef<SVGLineElement>(null)
  const liveRef = useRef(live)
  liveRef.current = live

  const path = useMemo(() => {
    if (!params.power) return ''
    const points: string[] = []
    for (let db = MIN_DB; db <= MAX_DB + 1e-6; db += 1) {
      const x = toX(db)
      const y = toY(outputDb(params, db))
      points.push(`${points.length === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`)
    }
    return points.join(' ')
  }, [params])

  const thresholdX = toX(circuitCurve(params).thresholdDb)

  useEffect(() => {
    let raf = 0
    const step = () => {
      const frame = metersRef.current
      const dot = dotRef.current
      const trace = traceRef.current
      if (dot && trace) {
        if (liveRef.current && frame && frame.inPeak > 1e-5) {
          const inDb = linearToDb(frame.inPeak)
          const outDb = linearToDb(Math.max(frame.outPeak, 1e-6))
          const x = toX(inDb)
          const y = toY(outDb)
          dot.setAttribute('cx', x.toFixed(2))
          dot.setAttribute('cy', y.toFixed(2))
          dot.style.display = ''
          trace.setAttribute('x1', x.toFixed(2))
          trace.setAttribute('y1', (PAD_T + PLOT_H).toFixed(2))
          trace.setAttribute('x2', x.toFixed(2))
          trace.setAttribute('y2', y.toFixed(2))
          trace.style.display = ''
        } else {
          dot.style.display = 'none'
          trace.style.display = 'none'
        }
      }
      raf = requestAnimationFrame(step)
    }
    raf = requestAnimationFrame(step)
    return () => cancelAnimationFrame(raf)
  }, [metersRef])

  return (
    <div className="curve">
      <svg viewBox={`0 0 ${VIEW_W} ${VIEW_H}`} role="img" aria-label="Transfer curve">
        <rect className="screen" x="0" y="0" width={VIEW_W} height={VIEW_H} rx="4" />

        {GRID_DB.map((db) => (
          <g key={db}>
            <line
              className={`grid${db === 0 ? ' edge' : ''}`}
              x1={toX(db)}
              y1={PAD_T}
              x2={toX(db)}
              y2={PAD_T + PLOT_H}
            />
            <line
              className={`grid${db === 0 ? ' edge' : ''}`}
              x1={PAD_L}
              y1={toY(db)}
              x2={PAD_L + PLOT_W}
              y2={toY(db)}
            />
          </g>
        ))}

        <line
          className="unity"
          x1={toX(MIN_DB)}
          y1={toY(MIN_DB)}
          x2={toX(MAX_DB)}
          y2={toY(MAX_DB)}
        />

        <line
          className="threshold"
          x1={thresholdX}
          y1={PAD_T}
          x2={thresholdX}
          y2={PAD_T + PLOT_H}
        />

        {path ? <path className="transfer" d={path} /> : null}

        <line ref={traceRef} className="trace" style={{ display: 'none' }} />
        <circle ref={dotRef} className="dot" r="2.6" style={{ display: 'none' }} />

        <text className="axis" x={PAD_L} y={VIEW_H - 6}>
          −60
        </text>
        <text className="axis" x={PAD_L + PLOT_W} y={VIEW_H - 6} textAnchor="end">
          0 dBFS
        </text>
        <text className="axis vert" x="10" y={PAD_T + 4} textAnchor="middle">
          OUT
        </text>
        <text className="caption" x={PAD_L + PLOT_W} y={PAD_T - 5} textAnchor="end">
          {params.power ? 'TRANSFER' : 'BYPASSED'}
        </text>
      </svg>
    </div>
  )
}
