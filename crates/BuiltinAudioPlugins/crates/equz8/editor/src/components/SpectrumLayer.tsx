import { useEffect, useRef, type RefObject } from 'react'
import type { SpectrumFrame } from '../bridge'
import { createSpectrumRenderer, type SpectrumRenderer } from '../lib/spectrumGl'

export type SpectrumLayerProps = {
  /// Live handle on the newest frame, written by the bridge listener. A ref and
  /// not a value: frames arrive at ~30 Hz, and passing them as props would
  /// rerender the whole graph — every curve, node and label — to repaint one
  /// canvas that can read the value itself.
  frameRef: RefObject<SpectrumFrame | null>
  /// Hidden while the unit is bypassed — the graph greys out with it, and a
  /// live analyser under a dead curve reads as if the plugin were still working.
  visible: boolean
}

/// The analyser overlay: a WebGL canvas sitting under the response SVG.
///
/// The redraw is driven by rAF, so the browser parks it entirely while the
/// editor window is hidden rather than burning frames nobody sees.
export function SpectrumLayer({ frameRef, visible }: SpectrumLayerProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const rendererRef = useRef<SpectrumRenderer | null>(null)
  const visibleRef = useRef(visible)

  /// Mirrored into a ref so the rAF loop can read it without the effect below
  /// re-running — rebuilding the GL context to flip a boolean would drop the
  /// context and reallocate the drawing buffer on every toggle.
  useEffect(() => {
    visibleRef.current = visible
  }, [visible])

  useEffect(() => {
    const canvas = canvasRef.current
    if (!canvas) return
    const renderer = createSpectrumRenderer(canvas)
    // No WebGL in this runtime: leave the canvas blank rather than falling back
    // to a drawing that is not the measured signal.
    if (!renderer) return
    rendererRef.current = renderer

    const observer = new ResizeObserver(() => renderer.resize())
    observer.observe(canvas)

    let raf = 0
    const tick = () => {
      renderer.draw(visibleRef.current ? frameRef.current : null)
      raf = requestAnimationFrame(tick)
    }
    raf = requestAnimationFrame(tick)

    return () => {
      cancelAnimationFrame(raf)
      observer.disconnect()
      renderer.dispose()
      rendererRef.current = null
    }
    // `frameRef` is a stable ref object owned by the editor root, so this
    // effect runs once: the GL context is built on mount and torn down on
    // unmount, never in between.
  }, [frameRef])

  return <canvas ref={canvasRef} className="spectrum" aria-hidden="true" />
}
