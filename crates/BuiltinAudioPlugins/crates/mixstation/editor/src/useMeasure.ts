import { useEffect, useRef, useState } from 'react'

/**
 * Measured bounds for a plot surface.
 *
 * Curve geometry is derived from real measured pixels rather than assumed
 * constants, so drawing and hit-testing share one coordinate space and the
 * editor survives native resize and DPI changes.
 */
export function useMeasure<T extends HTMLElement>() {
  const ref = useRef<T | null>(null)
  const [box, setBox] = useState({ w: 200, h: 64 })

  useEffect(() => {
    const node = ref.current
    if (!node) return
    const measure = () => {
      const rect = node.getBoundingClientRect()
      setBox((current) => {
        const w = Math.max(1, Math.round(rect.width))
        const h = Math.max(1, Math.round(rect.height))
        return current.w === w && current.h === h ? current : { w, h }
      })
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(node)
    return () => observer.disconnect()
  }, [])

  return [ref, box] as const
}
