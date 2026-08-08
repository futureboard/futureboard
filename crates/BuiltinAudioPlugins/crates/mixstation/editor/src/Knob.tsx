import {
  useEffect,
  useId,
  useRef,
  useState,
  type CSSProperties,
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
  size?: string
}

/** Degrees in canvas space (0 = 3 o'clock, CCW). Arc runs -225 → +45. */
const START_DEG = -225
const SWEEP_DEG = 270
const TRAVEL_PX = 230
const R = 41

function polar(deg: number, radius: number) {
  const rad = (deg * Math.PI) / 180
  return { x: 50 + Math.cos(rad) * radius, y: 50 + Math.sin(rad) * radius }
}

function arc(fromNorm: number, toNorm: number, radius: number) {
  const a0 = START_DEG + SWEEP_DEG * fromNorm
  const a1 = START_DEG + SWEEP_DEG * toNorm
  if (Math.abs(a1 - a0) < 0.08) return ''
  const p0 = polar(a0, radius)
  const p1 = polar(a1, radius)
  const large = Math.abs(a1 - a0) > 180 ? 1 : 0
  const sweep = a1 >= a0 ? 1 : 0
  return `M ${p0.x.toFixed(2)} ${p0.y.toFixed(2)} A ${radius} ${radius} 0 ${large} ${sweep} ${p1.x.toFixed(2)} ${p1.y.toFixed(2)}`
}

const TICKS = [0, 0.25, 0.5, 0.75, 1]

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
  if (spec.unit === ':1') return value.toFixed(1)
  return value.toFixed(1)
}

export function Knob({
  spec,
  value,
  onChange,
  disabled = false,
  size,
}: Props) {
  const uid = useId().replace(/:/g, '')
  const gesture = useRef<{ y: number; normalized: number } | null>(null)
  const [dragging, setDragging] = useState(false)
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState('')
  const inputRef = useRef<HTMLInputElement>(null)

  const normalized = normalizedValue(spec, value)
  const defaultNorm = normalizedValue(spec, spec.defaultValue)
  const display = formatParamValue(spec, value)
  const angle = START_DEG + SWEEP_DEG * normalized
  const pointer = polar(angle, R - 7)
  const inner = polar(angle, R - 24)
  const tip = polar(angle, R - 10)
  const defaultTick = polar(START_DEG + SWEEP_DEG * defaultNorm, 47)
  const defaultTickOuter = polar(START_DEG + SWEEP_DEG * defaultNorm, 51)
  const fill = arc(0, normalized, R)

  useEffect(() => {
    if (!editing) return
    inputRef.current?.focus()
    inputRef.current?.select()
  }, [editing])

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
    <div
      className={`knob${disabled ? ' is-disabled' : ''}`}
      style={
        size ? ({ ['--knob-size' as string]: size } as CSSProperties) : undefined
      }
    >
      <div className="knob-label">{spec.label}</div>
      <div
        className={`knob-dial${dragging ? ' is-dragging' : ''}`}
        role="slider"
        aria-label={spec.label}
        aria-valuemin={spec.min}
        aria-valuemax={spec.max}
        aria-valuenow={value}
        aria-valuetext={`${display} ${spec.unit}`}
        aria-disabled={disabled}
        tabIndex={disabled ? -1 : 0}
        title={`${spec.label} — drag vertically, Shift fine, double-click reset`}
        onPointerDown={(event) => {
          if (disabled || editing || event.button !== 0) return
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
        <svg viewBox="0 0 100 100" aria-hidden="true">
          <defs>
            <linearGradient id={`${uid}-ring`} x1="0" x2="0" y1="0" y2="1">
              <stop offset="0" stopColor="#33404a" />
              <stop offset="1" stopColor="#12171c" />
            </linearGradient>
            <linearGradient id={`${uid}-body`} x1="0" x2="0" y1="0" y2="1">
              <stop offset="0" stopColor="#2a333c" />
              <stop offset="0.45" stopColor="#171c22" />
              <stop offset="1" stopColor="#0c0f13" />
            </linearGradient>
            <radialGradient id={`${uid}-cap`} cx="0.32" cy="0.24" r="0.78">
              <stop offset="0" stopColor="#43515c" />
              <stop offset="0.28" stopColor="#273039" />
              <stop offset="0.7" stopColor="#14191f" />
              <stop offset="1" stopColor="#0a0d10" />
            </radialGradient>
            <radialGradient id={`${uid}-glow`} cx="0.5" cy="0.5" r="0.5">
              <stop offset="0" stopColor="#ffffff" stopOpacity="0.16" />
              <stop offset="1" stopColor="#ffffff" stopOpacity="0" />
            </radialGradient>
            <filter
              id={`${uid}-shadow`}
              x="-35%"
              y="-35%"
              width="170%"
              height="180%"
            >
              <feDropShadow
                dx="0"
                dy="3.5"
                stdDeviation="3.2"
                floodColor="#000"
                floodOpacity="0.55"
              />
            </filter>
          </defs>

          {TICKS.map((t) => {
            const deg = START_DEG + SWEEP_DEG * t
            const a = polar(deg, 46.5)
            const b = polar(deg, 49.5)
            return (
              <line
                key={t}
                className="knob-tick"
                x1={a.x}
                y1={a.y}
                x2={b.x}
                y2={b.y}
              />
            )
          })}

          <path className="knob-track" d={arc(0, 1, R)} />
          {fill ? <path className="knob-fill" d={fill} /> : null}
          <line
            className="knob-default"
            x1={defaultTick.x}
            y1={defaultTick.y}
            x2={defaultTickOuter.x}
            y2={defaultTickOuter.y}
          />

          <circle
            className="knob-ring"
            cx="50"
            cy="50"
            r="35.5"
            fill={`url(#${uid}-ring)`}
          />
          <circle
            className="knob-body"
            cx="50"
            cy="50"
            r="32.5"
            fill={`url(#${uid}-body)`}
            filter={`url(#${uid}-shadow)`}
          />
          <circle
            className="knob-cap"
            cx="50"
            cy="50"
            r="27.5"
            fill={`url(#${uid}-cap)`}
          />
          <circle className="knob-rim" cx="50" cy="50" r="27.5" />
          <path className="knob-highlight" d="M30 39 A 22 22 0 0 1 67 31" />
          {dragging ? (
            <circle cx="50" cy="50" r="36" fill={`url(#${uid}-glow)`} />
          ) : null}
          <line
            className="knob-pointer"
            x1={inner.x}
            y1={inner.y}
            x2={pointer.x}
            y2={pointer.y}
          />
          <circle className="knob-tip" cx={tip.x} cy={tip.y} r="2.1" />
        </svg>
      </div>

      {editing && !disabled ? (
        <input
          ref={inputRef}
          className="knob-input"
          value={draft}
          aria-label={`${spec.label} value`}
          onChange={(event) => setDraft(event.target.value)}
          onBlur={() => {
            const parsed = Number(draft.replace(/[^\d.+-]/g, ''))
            if (Number.isFinite(parsed)) {
              onChange(clamp(parsed, spec.min, spec.max))
            }
            setEditing(false)
          }}
          onKeyDown={(event) => {
            if (event.key === 'Enter') (event.target as HTMLInputElement).blur()
            if (event.key === 'Escape') setEditing(false)
          }}
        />
      ) : (
        <button
          type="button"
          className="knob-readout"
          disabled={disabled}
          title="Click to type a value"
          onClick={() => {
            if (disabled) return
            setDraft(String(Number(value.toFixed(2))))
            setEditing(true)
          }}
        >
          <span className="knob-value">{display}</span>
          <span className="knob-unit">{spec.unit}</span>
        </button>
      )}
    </div>
  )
}
