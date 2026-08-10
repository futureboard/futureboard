import { useEffect, useRef, useState } from 'react'

interface Props {
  height: number
  min: number
  max: number
  defaultHeight: number
  onChange: (h: number) => void
}

/** Drag bar between the transport and the band panel. Drag up to grow the panel. */
export function PanelResizer({ height, min, max, defaultHeight, onChange }: Props) {
  const [dragging, setDragging] = useState(false)
  const start = useRef<{ y: number; h: number } | null>(null)

  useEffect(() => {
    if (!dragging) return
    const move = (ev: PointerEvent) => {
      if (!start.current) return
      const next = start.current.h + (start.current.y - ev.clientY)
      onChange(Math.min(Math.max(next, min), max))
    }
    const up = () => {
      setDragging(false)
      start.current = null
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', up)
    return () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', up)
    }
  }, [dragging, min, max, onChange])

  return (
    <div
      onPointerDown={(ev) => {
        start.current = { y: ev.clientY, h: height }
        setDragging(true)
      }}
      onDoubleClick={() => onChange(defaultHeight)}
      title="Drag to resize the band panel · double-click to reset"
      className={`group flex h-3 shrink-0 cursor-ns-resize items-center justify-center border-t transition-colors ${
        dragging ? 'border-neon/50 bg-neon/20' : 'border-white/10 hover:bg-white/8'
      }`}
    >
      <div
        className={`h-0.5 w-10 rounded-full transition-colors ${
          dragging ? 'bg-neon shadow-[0_0_10px_rgba(255,77,157,0.8)]' : 'bg-white/15 group-hover:bg-white/40'
        }`}
      />
    </div>
  )
}
