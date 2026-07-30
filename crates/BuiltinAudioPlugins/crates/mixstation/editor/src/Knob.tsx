import {
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
  type WheelEvent,
} from 'react'
import {
  clamp,
  normalizedValue,
  valueFromNormalized,
  type ParamSpec,
} from './params'

type Props = {
  spec: ParamSpec
  value: number
  onChange: (value: number) => void
  disabled?: boolean
}

const START_DEG = -135
const SWEEP_DEG = 270
const TRAVEL_PX = 180

export function formatParamValue(spec: ParamSpec, value: number) {
  if (spec.unit === 'Hz') {
    return value >= 1_000
      ? `${(value / 1_000).toFixed(value >= 10_000 ? 0 : 1)}k`
      : String(Math.round(value))
  }
  if (spec.unit === 'ms') {
    return value < 10 ? value.toFixed(1) : String(Math.round(value))
  }
  if (spec.unit === '%') return String(Math.round(value))
  return value.toFixed(1)
}

export function Knob({ spec, value, onChange, disabled = false }: Props) {
  const gesture = useRef<{ y: number; normalized: number } | null>(null)
  const [dragging, setDragging] = useState(false)
  const normalized = normalizedValue(spec, value)
  const angle = START_DEG + normalized * SWEEP_DEG
  const display = formatParamValue(spec, value)

  const nudge = (direction: number, fine: boolean) => {
    const step = fine ? spec.step / 5 : spec.step
    onChange(clamp(value + direction * step, spec.min, spec.max))
  }

  const finish = (event: ReactPointerEvent<HTMLDivElement>) => {
    gesture.current = null
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId)
    }
    setDragging(false)
  }

  return (
    <div className={`knob-wrap${disabled ? ' disabled' : ''}`}>
      <div
        className={`knob-control${dragging ? ' dragging' : ''}`}
        role="slider"
        aria-label={spec.label}
        aria-valuemin={spec.min}
        aria-valuemax={spec.max}
        aria-valuenow={value}
        aria-valuetext={`${display} ${spec.unit}`}
        aria-disabled={disabled}
        tabIndex={disabled ? -1 : 0}
        title={`${spec.label}: drag vertically, Shift for fine, double-click to reset`}
        onPointerDown={(event) => {
          if (disabled || event.button !== 0) return
          gesture.current = { y: event.clientY, normalized }
          event.currentTarget.setPointerCapture(event.pointerId)
          setDragging(true)
          event.preventDefault()
        }}
        onPointerMove={(event) => {
          if (!gesture.current || disabled) return
          const travel = event.shiftKey ? TRAVEL_PX * 5 : TRAVEL_PX
          onChange(
            valueFromNormalized(
              spec,
              gesture.current.normalized +
                (gesture.current.y - event.clientY) / travel,
            ),
          )
        }}
        onPointerUp={finish}
        onPointerCancel={finish}
        onLostPointerCapture={() => {
          gesture.current = null
          setDragging(false)
        }}
        onDoubleClick={() => !disabled && onChange(spec.defaultValue)}
        onWheel={(event: WheelEvent<HTMLDivElement>) => {
          if (disabled) return
          event.preventDefault()
          nudge(event.deltaY < 0 ? 1 : -1, event.shiftKey)
        }}
        onKeyDown={(event) => {
          if (disabled) return
          if (
            event.key === 'ArrowUp' ||
            event.key === 'ArrowRight' ||
            event.key === 'ArrowDown' ||
            event.key === 'ArrowLeft'
          ) {
            event.preventDefault()
            nudge(
              event.key === 'ArrowUp' || event.key === 'ArrowRight' ? 1 : -1,
              event.shiftKey,
            )
          } else if (event.key === 'Home') {
            event.preventDefault()
            onChange(spec.defaultValue)
          }
        }}
      >
        <svg viewBox="0 0 72 72" aria-hidden="true">
          <circle className="knob-track" cx="36" cy="36" r="28" />
          <circle
            className="knob-progress"
            cx="36"
            cy="36"
            r="28"
            pathLength="100"
            strokeDasharray={`${normalized * 75} 100`}
          />
          <g style={{ transform: `rotate(${angle}deg)`, transformOrigin: '36px 36px' }}>
            <circle className="knob-face" cx="36" cy="36" r="22" />
            <path className="knob-mark" d="M36 16v11" />
          </g>
        </svg>
      </div>
      <span className="knob-label">{spec.label}</span>
      <output className="knob-value">
        {display}
        <small>{spec.unit}</small>
      </output>
    </div>
  )
}
