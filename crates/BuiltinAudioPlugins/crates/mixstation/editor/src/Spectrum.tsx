import { useEffect, useRef, type RefObject } from 'react'
import type { SpectrumFrame } from './bridge'

/**
 * Host analyser overlay, drawn behind the EQ curve.
 *
 * The frame is the signal arriving at the insert — the host captures it before
 * the DSP runs — so it shows what the EQ is being set against rather than the
 * result. Painted on canvas from a ref inside its own rAF loop, so ~30 Hz
 * analyser frames never invalidate the rack's React tree.
 *
 * Bins are log-spaced across `minHz..maxHz`, the same mapping as the editor's
 * log frequency axis, so bin `i` lands at its own frequency without resampling.
 */
export function SpectrumOverlay({
  frameRef,
  live,
  minHz,
  maxHz,
  accent,
}: {
  frameRef: RefObject<SpectrumFrame | null>
  live: boolean
  /** The plot's axis range, which may be narrower than the analyser's. */
  minHz: number
  maxHz: number
  accent: string
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null)
  const liveRef = useRef(live)
  liveRef.current = live

  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas) return
    const ctx = canvas.getContext('2d')
    if (!ctx) return

    let width = 0
    let height = 0
    const dpr = Math.min(window.devicePixelRatio || 1, 2)
    const resize = () => {
      const rect = canvas.getBoundingClientRect()
      width = Math.max(1, Math.round(rect.width))
      height = Math.max(1, Math.round(rect.height))
      canvas.width = width * dpr
      canvas.height = height * dpr
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
    }
    resize()
    const observer = new ResizeObserver(resize)
    observer.observe(canvas)

    // Decayed envelope so the display falls smoothly between analyser frames
    // instead of stepping at the publish rate.
    let envelope: Float32Array | null = null
    let raf = 0

    const paint = () => {
      raf = requestAnimationFrame(paint)
      ctx.clearRect(0, 0, width, height)
      const frame = liveRef.current ? frameRef.current : null
      if (!frame || frame.bins.length === 0) {
        envelope = null
        return
      }

      const count = frame.bins.length
      if (!envelope || envelope.length !== count) envelope = new Float32Array(count)

      const span = frame.ceilDb - frame.floorDb
      const logMin = Math.log(minHz)
      const logSpan = Math.log(maxHz) - logMin
      const binLogMin = Math.log(frame.minHz)
      const binLogSpan = Math.log(frame.maxHz) - binLogMin

      ctx.beginPath()
      ctx.moveTo(0, height)
      for (let i = 0; i < count; i++) {
        // Byte back to dB, then to a 0..1 height above the analyser floor.
        const db = frame.floorDb + (frame.bins[i]! / 255) * span
        const level = Math.max(0, Math.min(1, (db - frame.floorDb) / span))
        // Rise immediately, fall slowly — peaks stay readable at 60 fps.
        envelope[i] = level > envelope[i]! ? level : envelope[i]! * 0.88 + level * 0.12

        const hz = Math.exp(binLogMin + (binLogSpan * i) / (count - 1))
        const x = ((Math.log(hz) - logMin) / logSpan) * width
        ctx.lineTo(x, height - envelope[i]! * height)
      }
      ctx.lineTo(width, height)
      ctx.closePath()

      const gradient = ctx.createLinearGradient(0, height, 0, 0)
      gradient.addColorStop(0, 'rgba(255,255,255,0.02)')
      gradient.addColorStop(1, 'rgba(255,255,255,0.16)')
      ctx.fillStyle = gradient
      ctx.fill()
    }

    raf = requestAnimationFrame(paint)
    return () => {
      cancelAnimationFrame(raf)
      observer.disconnect()
    }
  }, [frameRef, minHz, maxHz, accent])

  return (
    <canvas
      ref={canvasRef}
      aria-hidden
      className="pointer-events-none absolute inset-0 h-full w-full"
    />
  )
}
