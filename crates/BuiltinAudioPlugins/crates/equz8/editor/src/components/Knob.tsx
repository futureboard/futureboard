import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from 'react'
import { clamp } from '../lib/eq'
import { snapKnobDial } from '../lib/motion'

const ANGLE_START = 135
const ANGLE_SWEEP = 270
const DRAG_SPAN = 210
const COARSE_STEP = 0.04
const FINE_STEP = 0.007

function polar(radius: number, progress: number) {
  const radians =
    ((ANGLE_START + ANGLE_SWEEP * clamp(progress, 0, 1)) * Math.PI) / 180
  return [
    50 + radius * Math.cos(radians),
    50 + radius * Math.sin(radians),
  ] as const
}

function arc(from: number, to: number, radius: number) {
  const start = polar(radius, from)
  const end = polar(radius, to)
  const large = Math.abs(to - from) * ANGLE_SWEEP > 180 ? 1 : 0
  const sweep = to >= from ? 1 : 0
  return `M ${start[0]} ${start[1]} A ${radius} ${radius} 0 ${large} ${sweep} ${end[0]} ${end[1]}`
}

export type KnobProps = {
  variant?: 'band' | 'master'
  label: string
  value: number
  min: number
  max: number
  step: number
  unit?: string
  format: (value: number) => string
  defaultValue: number
  originAtDefault?: boolean
  disabled?: boolean
  disabledHint?: string
  toProgress?: (value: number) => number
  fromProgress?: (progress: number) => number
  onChange: (value: number) => void
}

/** Serum-ish metallic dial on MixStation graphite chrome. */
export function Knob({
  variant = 'band',
  label,
  value,
  min,
  max,
  step,
  unit,
  format,
  defaultValue,
  originAtDefault,
  disabled,
  disabledHint,
  toProgress,
  fromProgress,
  onChange,
}: KnobProps) {
  const id = useId().replace(/:/g, '')
  const bodyGradientId = `${id}-body`
  const capGradientId = `${id}-cap`
  const shadowId = `${id}-shadow`
  const dialRef = useRef<HTMLDivElement>(null)
  const gesture = useRef<{ y: number; progress: number } | null>(null)
  const [dragging, setDragging] = useState(false)
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState('')

  const asProgress = useCallback(
    (raw: number) =>
      clamp(toProgress ? toProgress(raw) : (raw - min) / (max - min), 0, 1),
    [max, min, toProgress],
  )
  const asValue = useCallback(
    (progress: number) => {
      const raw = fromProgress
        ? fromProgress(progress)
        : min + progress * (max - min)
      return clamp(Math.round(raw / step) * step, min, max)
    },
    [fromProgress, max, min, step],
  )

  const progress = asProgress(value)
  const defaultProgress = asProgress(defaultValue)
  const fillOrigin = originAtDefault ? defaultProgress : 0
  const pointerStart = polar(14, progress)
  const pointerEnd = polar(27, progress)

  useEffect(() => {
    const dial = dialRef.current
    if (!dial || disabled) return
    const onWheel = (event: globalThis.WheelEvent) => {
      event.preventDefault()
      const amount = event.shiftKey ? FINE_STEP : COARSE_STEP
      onChange(
        asValue(
          clamp(progress + (event.deltaY < 0 ? amount : -amount), 0, 1),
        ),
      )
    }
    dial.addEventListener('wheel', onWheel, { passive: false })
    return () => dial.removeEventListener('wheel', onWheel)
  }, [asValue, disabled, onChange, progress])

  const commitValue = useCallback(
    (next: number, animateSnap = false) => {
      onChange(next)
      if (animateSnap) snapKnobDial(dialRef.current)
    },
    [onChange],
  )

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (disabled || editing) return
    event.preventDefault()
    gesture.current = { y: event.clientY, progress }
    event.currentTarget.setPointerCapture(event.pointerId)
    setDragging(true)
  }

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!gesture.current || disabled) return
    const delta = (gesture.current.y - event.clientY) / DRAG_SPAN
    onChange(
      asValue(
        clamp(
          gesture.current.progress + (event.shiftKey ? delta * 0.2 : delta),
          0,
          1,
        ),
      ),
    )
  }

  const endGesture = (event: ReactPointerEvent<HTMLDivElement>) => {
    gesture.current = null
    setDragging(false)
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId)
    }
  }

  const commitEdit = () => {
    const parsed = Number(draft.replace(/[^\d.+-]/g, ''))
    if (Number.isFinite(parsed)) commitValue(clamp(parsed, min, max), true)
    setEditing(false)
  }

  return (
    <div
      className={`knob is-${variant} flex min-w-0 flex-col items-center gap-1 ${
        dragging ? 'is-dragging' : ''
      } ${disabled ? 'pointer-events-none opacity-30' : ''}`}
      style={
        {
          '--control-accent':
            variant === 'master' ? 'var(--color-signal)' : 'var(--band)',
        } as Record<string, string>
      }
      title={disabled ? disabledHint : undefined}
    >
      <span className="label-cap">{label}</span>
      <div
        ref={dialRef}
        className="knob-dial h-14 w-14 cursor-ns-resize touch-none outline-none sm:h-16 sm:w-16"
        role="slider"
        tabIndex={disabled ? -1 : 0}
        aria-label={label}
        aria-valuemin={min}
        aria-valuemax={max}
        aria-valuenow={value}
        aria-valuetext={`${format(value)}${unit ? ` ${unit}` : ''}`}
        aria-disabled={disabled}
        title={
          disabled
            ? disabledHint
            : `${label} — drag vertically, Shift for fine, double-click to reset`
        }
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endGesture}
        onPointerCancel={endGesture}
        onDoubleClick={() => !disabled && commitValue(defaultValue, true)}
        onContextMenu={(event) => {
          event.preventDefault()
          if (!disabled) commitValue(defaultValue, true)
        }}
        onKeyDown={(event) => {
          if (disabled) return
          const amount = event.shiftKey ? FINE_STEP : COARSE_STEP
          if (event.key === 'ArrowUp' || event.key === 'ArrowRight') {
            event.preventDefault()
            onChange(asValue(clamp(progress + amount, 0, 1)))
          }
          if (event.key === 'ArrowDown' || event.key === 'ArrowLeft') {
            event.preventDefault()
            onChange(asValue(clamp(progress - amount, 0, 1)))
          }
          if (event.key === 'Home') {
            event.preventDefault()
            commitValue(min, true)
          }
          if (event.key === 'End') {
            event.preventDefault()
            commitValue(max, true)
          }
        }}
      >
        <svg viewBox="0 0 100 100" className="block h-full w-full overflow-visible" aria-hidden="true">
          <defs>
            <linearGradient id={bodyGradientId} x1="0" x2="0" y1="0" y2="1">
              <stop offset="0" stopColor="#323b48" />
              <stop offset=".45" stopColor="#1a212c" />
              <stop offset="1" stopColor="#0a0d12" />
            </linearGradient>
            <radialGradient id={capGradientId} cx=".32" cy=".28" r=".8">
              <stop offset="0" stopColor="#3a4454" />
              <stop offset=".4" stopColor="#222a36" />
              <stop offset="1" stopColor="#10151c" />
            </radialGradient>
            <filter id={shadowId} x="-30%" y="-30%" width="160%" height="170%">
              <feDropShadow
                dx="0"
                dy="3"
                stdDeviation="3.5"
                floodColor="#000"
                floodOpacity=".55"
              />
            </filter>
          </defs>
          <circle className="knob-tick-ring" cx="50" cy="50" r="44" />
          <path className="knob-track" d={arc(0, 1, 42)} />
          <path className="knob-fill" d={arc(fillOrigin, progress, 42)} />
          <line
            className="knob-default"
            x1={polar(45, defaultProgress)[0]}
            y1={polar(45, defaultProgress)[1]}
            x2={polar(49, defaultProgress)[0]}
            y2={polar(49, defaultProgress)[1]}
          />
          <circle
            className="knob-body"
            cx="50"
            cy="50"
            r="33"
            fill={`url(#${bodyGradientId})`}
            filter={`url(#${shadowId})`}
          />
          <circle
            className="knob-cap"
            cx="50"
            cy="50"
            r="28"
            fill={`url(#${capGradientId})`}
          />
          <path className="knob-highlight" d="M32 37 A 22 22 0 0 1 67 31" />
          <line
            className="knob-pointer"
            x1={pointerStart[0]}
            y1={pointerStart[1]}
            x2={pointerEnd[0]}
            y2={pointerEnd[1]}
          />
          <circle className="knob-center" cx="50" cy="50" r="4.5" />
        </svg>
      </div>

      {editing ? (
        <input
          className="h-5 w-16 rounded border border-hairline-hi bg-well text-center text-[11px] text-ink outline-none"
          autoFocus
          value={draft}
          aria-label={`${label} value`}
          onChange={(event) => setDraft(event.target.value)}
          onBlur={commitEdit}
          onKeyDown={(event) => {
            if (event.key === 'Enter') commitEdit()
            if (event.key === 'Escape') setEditing(false)
          }}
        />
      ) : (
        <button
          type="button"
          className="readout flex min-h-5 min-w-14 cursor-text items-baseline justify-center gap-1 rounded px-1 hover:bg-white/5"
          disabled={disabled}
          title="Click to type a value"
          onClick={() => {
            setDraft(String(Number(value.toFixed(2))))
            setEditing(true)
          }}
        >
          <span>{format(value)}</span>
          {unit ? <i className="label-cap not-italic opacity-70">{unit}</i> : null}
        </button>
      )}
    </div>
  )
}
