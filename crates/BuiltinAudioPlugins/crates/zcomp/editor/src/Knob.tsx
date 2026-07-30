/**
 * Machined aluminum knob: engraved scale collar, knurled skirt, value plate.
 *
 * Drawing and hit-testing share one transform — the pointer angle is derived
 * from the same normalised value the collar arc is drawn from, so what the user
 * sees and what the gesture reports can never disagree.
 */

import {
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from 'react'
import { KNOB_START_DEG, KNOB_SWEEP_DEG, KNOB_TRAVEL_PX } from './device'
import { clamp } from './meter'
import type { ParamSpec } from './params'

type KnobProps = {
  spec: ParamSpec
  value: number
  display: (value: number) => string
  onChange: (value: number) => void
  size?: 'lg' | 'sm'
  disabled?: boolean
  disabledReadout?: string
  /** Engraved end-of-travel legends, e.g. `['-60', '0']`. */
  scale?: readonly [string, string]
}

const VIEW = 120
const CENTRE = VIEW / 2
const COLLAR_R = 52
const SKIRT_R = 37
const CAP_R = 30

function polar(angleDeg: number, radius: number) {
  const rad = ((angleDeg - 90) * Math.PI) / 180
  return {
    x: CENTRE + Math.cos(rad) * radius,
    y: CENTRE + Math.sin(rad) * radius,
  }
}

function arc(fromDeg: number, toDeg: number, radius: number) {
  const a = polar(fromDeg, radius)
  const b = polar(toDeg, radius)
  const large = Math.abs(toDeg - fromDeg) > 180 ? 1 : 0
  return `M ${a.x.toFixed(2)} ${a.y.toFixed(2)} A ${radius} ${radius} 0 ${large} 1 ${b.x.toFixed(2)} ${b.y.toFixed(2)}`
}

/** Eleven engraved marks around the collar, every fifth one major. */
const TICKS = Array.from({ length: 11 }, (_, i) => i / 10)

/** Knurling: 44 flutes cut into the skirt, rotating with the knob. */
const FLUTES = Array.from({ length: 44 }, (_, i) => (i * 360) / 44)

export function Knob({
  spec,
  value,
  display,
  onChange,
  size = 'lg',
  disabled = false,
  disabledReadout,
  scale,
}: KnobProps) {
  const uid = useId().replace(/:/g, '')
  const gesture = useRef<{ y: number; norm: number } | null>(null)
  const [dragging, setDragging] = useState(false)
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState('')
  const inputRef = useRef<HTMLInputElement>(null)

  const { min, max, step, label, unit, defaultValue } = spec
  const norm = clamp((value - min) / Math.max(max - min, 1e-9), 0, 1)
  const angle = KNOB_START_DEG + KNOB_SWEEP_DEG * norm

  const fromNorm = useCallback(
    (n: number) => {
      const raw = min + clamp(n, 0, 1) * (max - min)
      return clamp(Math.round(raw / step) * step, min, max)
    },
    [max, min, step],
  )

  useEffect(() => {
    if (!editing) return
    inputRef.current?.focus()
    inputRef.current?.select()
  }, [editing])

  const flutes = useMemo(
    () =>
      FLUTES.map((deg) => {
        const outer = polar(deg, SKIRT_R)
        const inner = polar(deg, SKIRT_R - 5)
        return { deg, outer, inner }
      }),
    [],
  )

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (disabled || editing || event.button !== 0) return
    gesture.current = { y: event.clientY, norm }
    event.currentTarget.setPointerCapture(event.pointerId)
    setDragging(true)
    event.preventDefault()
  }

  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!gesture.current || disabled) return
    // Shift is the fine-adjust gesture everywhere in Futureboard.
    const travel = event.shiftKey ? KNOB_TRAVEL_PX * 5 : KNOB_TRAVEL_PX
    onChange(
      fromNorm(gesture.current.norm + (gesture.current.y - event.clientY) / travel),
    )
  }

  const endGesture = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!gesture.current) return
    gesture.current = null
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId)
    }
    setDragging(false)
  }

  const readout =
    disabled && disabledReadout ? (
      <span className="auto">{disabledReadout}</span>
    ) : (
      <>
        {display(value)}
        <span className="unit">{unit}</span>
      </>
    )

  return (
    <div className={`knob ${size}${disabled ? ' is-disabled' : ''}`}>
      <div
        className={`cap${dragging ? ' is-dragging' : ''}`}
        role="slider"
        aria-label={label}
        aria-valuemin={min}
        aria-valuemax={max}
        aria-valuenow={value}
        aria-valuetext={
          disabled && disabledReadout ? disabledReadout : `${display(value)} ${unit}`
        }
        aria-disabled={disabled}
        tabIndex={disabled ? -1 : 0}
        title={
          disabled
            ? (disabledReadout ?? `${label} is programmed by the circuit`)
            : `${label} — drag to set, Shift for fine, double-click to reset`
        }
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endGesture}
        onPointerCancel={endGesture}
        onLostPointerCapture={() => {
          gesture.current = null
          setDragging(false)
        }}
        onDoubleClick={() => !disabled && onChange(defaultValue)}
        onWheel={(event) => {
          if (disabled) return
          const delta = event.shiftKey ? step / 5 : step
          onChange(clamp(value + (event.deltaY < 0 ? delta : -delta), min, max))
        }}
        onKeyDown={(event) => {
          if (disabled) return
          const delta = event.shiftKey ? step / 5 : step
          if (event.key === 'ArrowUp' || event.key === 'ArrowRight') {
            event.preventDefault()
            onChange(clamp(value + delta, min, max))
          } else if (event.key === 'ArrowDown' || event.key === 'ArrowLeft') {
            event.preventDefault()
            onChange(clamp(value - delta, min, max))
          } else if (event.key === 'Home') {
            event.preventDefault()
            onChange(defaultValue)
          }
        }}
      >
        <svg viewBox={`0 0 ${VIEW} ${VIEW}`} aria-hidden="true">
          <defs>
            <linearGradient id={`${uid}-skirt`} x1="0" x2="0" y1="0" y2="1">
              <stop offset="0" stopColor="#5c6472" />
              <stop offset="0.45" stopColor="#2b313a" />
              <stop offset="1" stopColor="#13171d" />
            </linearGradient>
            <linearGradient id={`${uid}-cap`} x1="0.15" x2="0.85" y1="0" y2="1">
              <stop offset="0" stopColor="#e4e9f1" />
              <stop offset="0.28" stopColor="#a8b1bf" />
              <stop offset="0.52" stopColor="#6e7684" />
              <stop offset="0.74" stopColor="#99a2b0" />
              <stop offset="1" stopColor="#4a515c" />
            </linearGradient>
            <radialGradient id={`${uid}-dish`} cx="0.35" cy="0.3" r="0.8">
              <stop offset="0" stopColor="rgba(255,255,255,0.35)" />
              <stop offset="0.6" stopColor="rgba(255,255,255,0.04)" />
              <stop offset="1" stopColor="rgba(0,0,0,0.35)" />
            </radialGradient>
          </defs>

          {/* Engraved collar: travel track, live value, tick marks. */}
          <path
            className="collar-track"
            d={arc(KNOB_START_DEG, KNOB_START_DEG + KNOB_SWEEP_DEG, COLLAR_R)}
          />
          {norm > 0.001 ? (
            <path
              className="collar-value"
              d={arc(KNOB_START_DEG, angle, COLLAR_R)}
            />
          ) : null}
          {TICKS.map((t) => {
            const deg = KNOB_START_DEG + KNOB_SWEEP_DEG * t
            const major = Math.round(t * 10) % 5 === 0
            const outer = polar(deg, COLLAR_R + 6)
            const inner = polar(deg, COLLAR_R + (major ? 1 : 3))
            return (
              <line
                key={t}
                className={`tick${major ? ' major' : ''}`}
                x1={inner.x}
                y1={inner.y}
                x2={outer.x}
                y2={outer.y}
              />
            )
          })}

          {/* Knob body. Everything inside rotates as one rigid part. */}
          <g
            style={{
              transform: `rotate(${angle}deg)`,
              transformOrigin: `${CENTRE}px ${CENTRE}px`,
            }}
          >
            <circle
              className="skirt"
              cx={CENTRE}
              cy={CENTRE}
              r={SKIRT_R}
              fill={`url(#${uid}-skirt)`}
            />
            <g className="flutes">
              {flutes.map((flute) => (
                <line
                  key={flute.deg}
                  x1={flute.inner.x}
                  y1={flute.inner.y}
                  x2={flute.outer.x}
                  y2={flute.outer.y}
                />
              ))}
            </g>
            <circle
              className="face"
              cx={CENTRE}
              cy={CENTRE}
              r={CAP_R}
              fill={`url(#${uid}-cap)`}
            />
            <circle
              cx={CENTRE}
              cy={CENTRE}
              r={CAP_R}
              fill={`url(#${uid}-dish)`}
              pointerEvents="none"
            />
            <rect
              className="pointer"
              x={CENTRE - 1.7}
              y={CENTRE - CAP_R - 1}
              width="3.4"
              height="17"
              rx="1.7"
            />
            <circle className="hub" cx={CENTRE} cy={CENTRE} r="3.6" />
          </g>
        </svg>

        {scale ? (
          <>
            <span className="scale-min">{scale[0]}</span>
            <span className="scale-max">{scale[1]}</span>
          </>
        ) : null}
      </div>

      <div className="legend">{label}</div>

      {editing && !disabled ? (
        <input
          ref={inputRef}
          className="entry"
          value={draft}
          aria-label={`${label} value`}
          onChange={(event) => setDraft(event.target.value)}
          onBlur={() => {
            const parsed = Number(draft.replace(/[^\d.+-]/g, ''))
            if (Number.isFinite(parsed)) onChange(clamp(parsed, min, max))
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
          className="plate"
          disabled={disabled}
          title={disabled ? disabledReadout : 'Click to type a value'}
          onClick={() => {
            if (disabled) return
            setDraft(String(Number(value.toFixed(2))))
            setEditing(true)
          }}
        >
          {readout}
        </button>
      )}
    </div>
  )
}
