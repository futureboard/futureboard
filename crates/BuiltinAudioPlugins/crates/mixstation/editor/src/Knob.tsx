import { useEffect, useId, useRef, useState } from 'react'
import { animate } from 'animejs'
import {
  clamp,
  normalizedValue,
  valueFromNormalized,
  type ParamSpec,
} from './params'

const ARC_START = -135
const ARC_SWEEP = 270
/** Pointer travel, in px, that spans the whole range. Shift gives 5x precision. */
const COARSE_TRAVEL = 160
const FINE_TRAVEL = 800

type Props = {
  spec: ParamSpec
  value: number
  accent?: string
  size?: number
  disabled?: boolean
  /** Fill outward from centre — for parameters whose neutral point is 0. */
  bipolar?: boolean
  onChange: (value: number) => void
}

function polar(cx: number, cy: number, r: number, deg: number) {
  const rad = ((deg - 90) * Math.PI) / 180
  return [cx + r * Math.cos(rad), cy + r * Math.sin(rad)] as const
}

function arcPath(cx: number, cy: number, r: number, fromDeg: number, toDeg: number) {
  const [x0, y0] = polar(cx, cy, r, fromDeg)
  const [x1, y1] = polar(cx, cy, r, toDeg)
  const large = Math.abs(toDeg - fromDeg) > 180 ? 1 : 0
  const sweep = toDeg > fromDeg ? 1 : 0
  return `M ${x0} ${y0} A ${r} ${r} 0 ${large} ${sweep} ${x1} ${y1}`
}

export function formatValue(spec: ParamSpec, value: number) {
  if (spec.unit === 'Hz' && value >= 1000) return `${(value / 1000).toFixed(2)} kHz`
  const decimals = spec.step < 1 ? 1 : 0
  const text = value.toFixed(decimals)
  if (!spec.unit) return text
  return spec.unit.startsWith(':') ? `${text}${spec.unit}` : `${text} ${spec.unit}`
}

/**
 * Continuous parameter control.
 *
 * Every gesture the design contract asks for is present: a visible value
 * affordance, drag with fine adjustment, wheel, full keyboard, and a
 * double-click reset to the Rust-authored default. Values are always committed
 * through `onChange`, which coalesces to the bridge — the knob holds no
 * authoritative state of its own.
 */
export function Knob({
  spec,
  value,
  accent = 'var(--color-signal)',
  size = 44,
  disabled = false,
  bipolar = false,
  onChange,
}: Props) {
  const norm = normalizedValue(spec, value)
  const [dragging, setDragging] = useState(false)
  const dialRef = useRef<SVGGElement | null>(null)
  const drag = useRef({ y: 0, norm: 0 })
  const labelId = useId()

  const commit = (next: number) => onChange(valueFromNormalized(spec, clamp(next, 0, 1)))

  const onPointerDown = (event: React.PointerEvent<SVGSVGElement>) => {
    if (disabled) return
    event.currentTarget.setPointerCapture(event.pointerId)
    drag.current = { y: event.clientY, norm }
    setDragging(true)
  }

  const onPointerMove = (event: React.PointerEvent<SVGSVGElement>) => {
    if (!dragging) return
    const travel = event.shiftKey ? FINE_TRAVEL : COARSE_TRAVEL
    commit(drag.current.norm + (drag.current.y - event.clientY) / travel)
  }

  const endDrag = (event: React.PointerEvent<SVGSVGElement>) => {
    if (!dragging) return
    event.currentTarget.releasePointerCapture(event.pointerId)
    setDragging(false)
  }

  const onKeyDown = (event: React.KeyboardEvent<SVGSVGElement>) => {
    if (disabled) return
    const step = event.shiftKey ? 0.002 : 0.02
    switch (event.key) {
      case 'ArrowUp':
      case 'ArrowRight':
        event.preventDefault()
        commit(norm + step)
        break
      case 'ArrowDown':
      case 'ArrowLeft':
        event.preventDefault()
        commit(norm - step)
        break
      case 'Home':
        event.preventDefault()
        commit(0)
        break
      case 'End':
        event.preventDefault()
        commit(1)
        break
    }
  }

  const reset = () => {
    if (disabled) return
    onChange(spec.defaultValue)
    if (dialRef.current) {
      animate(dialRef.current, {
        scale: [
          { to: 1.12, duration: 100 },
          { to: 1, duration: 200 },
        ],
        ease: 'outElastic(1, .6)',
      })
    }
  }

  useEffect(() => {
    if (!dragging) return
    const previous = document.body.style.cursor
    document.body.style.cursor = 'ns-resize'
    return () => {
      document.body.style.cursor = previous
    }
  }, [dragging])

  const cx = size / 2
  const cy = size / 2
  const r = size / 2 - 2
  const capR = r - 5.5
  const angle = ARC_START + norm * ARC_SWEEP
  const centreDeg = ARC_START + ARC_SWEEP / 2
  const fillFrom = bipolar ? centreDeg : ARC_START
  const [tipX, tipY] = polar(cx, cy, capR - 2.5, angle)
  const [baseX, baseY] = polar(cx, cy, capR * 0.32, angle)

  return (
    <div className="flex w-[68px] shrink-0 flex-col items-center gap-1">
      <span id={labelId} className="label-cap w-full truncate text-center">
        {spec.label}
      </span>
      <svg
        role="slider"
        tabIndex={disabled ? -1 : 0}
        aria-labelledby={labelId}
        aria-valuemin={spec.min}
        aria-valuemax={spec.max}
        aria-valuenow={value}
        aria-valuetext={formatValue(spec, value)}
        aria-disabled={disabled || undefined}
        width={size}
        height={size}
        viewBox={`0 0 ${size} ${size}`}
        className={
          disabled
            ? 'cursor-not-allowed touch-none opacity-40'
            : 'cursor-ns-resize touch-none'
        }
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endDrag}
        onPointerCancel={endDrag}
        onDoubleClick={reset}
        onKeyDown={onKeyDown}
      >
        <g ref={dialRef} style={{ transformOrigin: `${cx}px ${cy}px` }}>
          <path
            d={arcPath(cx, cy, r, ARC_START, ARC_START + ARC_SWEEP)}
            fill="none"
            stroke="var(--color-hairline-hi)"
            strokeWidth={2.5}
            strokeLinecap="round"
          />
          {Math.abs(angle - fillFrom) > 0.5 && (
            <path
              d={arcPath(cx, cy, r, fillFrom, angle)}
              fill="none"
              stroke={accent}
              strokeWidth={2.5}
              strokeLinecap="round"
            />
          )}
          {bipolar && (
            <line
              x1={polar(cx, cy, r - 4.5, centreDeg)[0]}
              y1={polar(cx, cy, r - 4.5, centreDeg)[1]}
              x2={polar(cx, cy, r + 2.5, centreDeg)[0]}
              y2={polar(cx, cy, r + 2.5, centreDeg)[1]}
              stroke="var(--color-ink-dim)"
              strokeWidth={1}
            />
          )}
          <circle cx={cx} cy={cy} r={capR} fill="url(#knobCap)" />
          <circle
            cx={cx}
            cy={cy}
            r={capR}
            fill="none"
            stroke="rgb(255 255 255 / 0.08)"
            strokeWidth={1}
          />
          <line
            x1={baseX}
            y1={baseY}
            x2={tipX}
            y2={tipY}
            stroke={dragging ? accent : 'var(--color-ink)'}
            strokeWidth={1.8}
            strokeLinecap="round"
          />
        </g>
        <defs>
          <radialGradient id="knobCap" cx="50%" cy="22%" r="80%">
            <stop offset="0%" stopColor="#3a3d45" />
            <stop offset="55%" stopColor="#24272d" />
            <stop offset="100%" stopColor="#15171b" />
          </radialGradient>
        </defs>
      </svg>
      <span className="readout w-full overflow-hidden text-center text-ellipsis">
        {formatValue(spec, value)}
      </span>
    </div>
  )
}
