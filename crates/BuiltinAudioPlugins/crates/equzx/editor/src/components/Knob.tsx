import { useCallback, useEffect, useRef, useState } from 'react'
import { NEON, SURFACE_HUB } from '../theme'

interface Props {
  label: string
  value: number
  min: number
  max: number
  /** Log taper suits frequency and Q; linear suits gain. */
  scale?: 'linear' | 'log'
  unit?: string
  decimals?: number
  defaultValue?: number
  color?: string
  disabled?: boolean
  /** Diameter of the dial in px. Everything inside scales with it. */
  size?: number
  /**
   * 'stacked' is the band-panel form — dial over value over label. 'inline' is
   * the compact one for a header pill: dial beside a two-line caption.
   */
  layout?: 'stacked' | 'inline'
  format?: (v: number) => string
  onChange: (v: number) => void
}

const ARC = 270 // degrees of travel
const START = -135

function clamp(v: number, lo: number, hi: number) {
  return Math.min(Math.max(v, lo), hi)
}

export function Knob({
  label,
  value,
  min,
  max,
  scale = 'linear',
  unit = '',
  decimals = 1,
  defaultValue,
  color = NEON,
  disabled = false,
  size = 48,
  layout = 'stacked',
  format,
  onChange,
}: Props) {
  const [dragging, setDragging] = useState(false)
  const drag = useRef<{ y: number; norm: number } | null>(null)

  const toNorm = useCallback(
    (v: number) =>
      scale === 'log'
        ? Math.log(clamp(v, min, max) / min) / Math.log(max / min)
        : (clamp(v, min, max) - min) / (max - min),
    [min, max, scale],
  )
  const fromNorm = useCallback(
    (n: number) =>
      scale === 'log' ? min * Math.pow(max / min, clamp(n, 0, 1)) : min + clamp(n, 0, 1) * (max - min),
    [min, max, scale],
  )

  const norm = toNorm(value)

  useEffect(() => {
    if (!dragging) return
    const move = (ev: PointerEvent) => {
      if (!drag.current) return
      const speed = ev.shiftKey ? 0.0008 : 0.004
      const next = clamp(drag.current.norm + (drag.current.y - ev.clientY) * speed, 0, 1)
      onChange(fromNorm(next))
    }
    const up = () => {
      setDragging(false)
      drag.current = null
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', up)
    return () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', up)
    }
  }, [dragging, fromNorm, onChange])

  const angle = START + norm * ARC
  // Everything is expressed as a fraction of the dial so any size stays in proportion.
  const c = size / 2
  const r = size * 0.354
  const hub = size * 0.25
  const pointer = size * 0.208
  const stroke = Math.max(2, size * 0.0625)
  const arcPath = (fromNormValue: number, toNormValue: number) => {
    const a0 = ((START + fromNormValue * ARC) * Math.PI) / 180
    const a1 = ((START + toNormValue * ARC) * Math.PI) / 180
    const p = (a: number) => [c + r * Math.sin(a), c - r * Math.cos(a)]
    const [x0, y0] = p(a0)
    const [x1, y1] = p(a1)
    const large = Math.abs(a1 - a0) > Math.PI ? 1 : 0
    const sweep = a1 > a0 ? 1 : 0
    return `M${x0.toFixed(2)},${y0.toFixed(2)} A${r},${r} 0 ${large} ${sweep} ${x1.toFixed(2)},${y1.toFixed(2)}`
  }

  // Bipolar params (gain) fill outward from centre; unipolar fill from the left.
  const bipolar = min < 0 && max > 0 && scale === 'linear'
  const originNorm = bipolar ? toNorm(0) : 0

  const text = format ? format(value) : `${value.toFixed(decimals)}${unit}`

  const dial = (
    <svg
      width={size}
      height={size}
      className={`shrink-0 ${
        disabled ? 'cursor-not-allowed' : dragging ? 'cursor-grabbing' : 'cursor-grab'
      }`}
      onPointerDown={(ev) => {
        if (disabled) return
        ;(ev.target as Element).setPointerCapture?.(ev.pointerId)
        drag.current = { y: ev.clientY, norm }
        setDragging(true)
      }}
      onDoubleClick={() => !disabled && defaultValue !== undefined && onChange(defaultValue)}
    >
      <path
        d={arcPath(0, 1)}
        fill="none"
        stroke="rgba(255,255,255,0.10)"
        strokeWidth={stroke}
        strokeLinecap="round"
      />
      <path
        d={arcPath(Math.min(originNorm, norm), Math.max(originNorm, norm))}
        fill="none"
        stroke={color}
        strokeWidth={stroke}
        strokeLinecap="round"
        opacity={0.95}
      />
      <circle cx={c} cy={c} r={hub} fill={SURFACE_HUB} stroke="rgba(255,255,255,0.08)" />
      <line
        x1={c}
        y1={c}
        x2={c + pointer * Math.sin((angle * Math.PI) / 180)}
        y2={c - pointer * Math.cos((angle * Math.PI) / 180)}
        stroke={color}
        strokeWidth={Math.max(1.5, stroke * 0.67)}
        strokeLinecap="round"
      />
    </svg>
  )

  if (layout === 'inline') {
    return (
      <div className={`flex items-center gap-1.5 ${disabled ? 'opacity-30' : ''}`}>
        {dial}
        <div className="flex flex-col leading-tight">
          <span className="text-[9px] font-medium uppercase tracking-[0.14em] text-white/35">
            {label}
          </span>
          <span className="text-[11px] font-medium tabular-nums text-white/90">{text}</span>
        </div>
      </div>
    )
  }

  return (
    <div className={`flex w-16 flex-col items-center gap-1 ${disabled ? 'opacity-30' : ''}`}>
      {dial}
      <div className="text-[10px] font-medium tabular-nums text-white/85">{text}</div>
      <div className="text-[9px] uppercase tracking-wider text-white/35">{label}</div>
    </div>
  )
}
