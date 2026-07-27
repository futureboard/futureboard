<script lang="ts">
  import type { VerbParams } from '../bridge'
  import { decayModel, diffusionGain, levelDbAt } from '../model'

  type Props = { params: VerbParams }
  const { params }: Props = $props()

  let canvas: HTMLCanvasElement | undefined = $state()
  let host: HTMLDivElement | undefined = $state()
  let cssWidth = $state(0)
  let cssHeight = $state(0)

  /** Floor of the dB axis. Reverb tails are quoted to -60, so is this. */
  const FLOOR_DB = -60

  /** Longest window the axis will show, so a 20 s decay does not squash a
   *  20 ms pre-delay into the first pixel. */
  const MAX_WINDOW_SEC = 8

  const model = $derived(decayModel(params))

  const windowSec = $derived(
    Math.min(
      MAX_WINDOW_SEC,
      Math.max(
        0.35,
        params.predelayMs / 1000 +
          (Number.isFinite(model.longestSec) ? model.longestSec : 2) * 1.08,
      ),
    ),
  )

  function cssVar(name: string, fallback: string): string {
    if (!host) return fallback
    const value = getComputedStyle(host).getPropertyValue(name).trim()
    return value || fallback
  }

  /** `#rrggbb` + alpha -> `rgba(...)`, so one palette drives both the DOM and
   *  the canvas instead of the canvas carrying a second set of literals. */
  function alpha(hex: string, a: number): string {
    const match = /^#?([0-9a-f]{6})$/i.exec(hex.trim())
    if (!match?.[1]) return hex
    const int = parseInt(match[1], 16)
    return `rgba(${(int >> 16) & 255}, ${(int >> 8) & 255}, ${int & 255}, ${a})`
  }

  function draw() {
    if (!canvas || cssWidth <= 0 || cssHeight <= 0) return
    const ctx = canvas.getContext('2d')
    if (!ctx) return

    const dpr = window.devicePixelRatio || 1
    canvas.width = Math.round(cssWidth * dpr)
    canvas.height = Math.round(cssHeight * dpr)
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0)
    ctx.clearRect(0, 0, cssWidth, cssHeight)

    const accent = cssVar('--accent', '#9b7dff')
    const accentBright = cssVar('--accent-bright', '#b9a0ff')
    const warn = cssVar('--warn', '#f0a83c')
    const grid = cssVar('--grid', '#241f2c')
    const faint = cssVar('--text-faint', '#6d647a')

    const padL = 38
    const padR = 14
    const padT = 14
    const padB = 24
    const w = Math.max(cssWidth - padL - padR, 1)
    const h = Math.max(cssHeight - padT - padB, 1)

    const x = (t: number) => padL + (t / windowSec) * w
    const y = (db: number) =>
      padT + (Math.min(0, Math.max(FLOOR_DB, db)) / FLOOR_DB) * h

    // ---- grid ------------------------------------------------------------
    ctx.font =
      '10px ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace'
    ctx.textBaseline = 'middle'
    ctx.lineWidth = 1

    for (const db of [-12, -24, -36, -48, -60]) {
      const gy = Math.round(y(db)) + 0.5
      ctx.strokeStyle = grid
      ctx.beginPath()
      ctx.moveTo(padL, gy)
      ctx.lineTo(padL + w, gy)
      ctx.stroke()
      ctx.fillStyle = faint
      ctx.textAlign = 'right'
      ctx.fillText(`${db}`, padL - 6, gy)
    }

    const tickSec =
      windowSec <= 0.6 ? 0.1 : windowSec <= 2 ? 0.25 : windowSec <= 5 ? 1 : 2
    ctx.textAlign = 'center'
    ctx.textBaseline = 'top'
    for (let t = tickSec; t < windowSec; t += tickSec) {
      const gx = Math.round(x(t)) + 0.5
      ctx.strokeStyle = grid
      ctx.beginPath()
      ctx.moveTo(gx, padT)
      ctx.lineTo(gx, padT + h)
      ctx.stroke()
      ctx.fillStyle = faint
      ctx.fillText(
        tickSec < 1 ? `${Math.round(t * 1000)}ms` : `${t.toFixed(0)}s`,
        gx,
        padT + h + 5,
      )
    }

    // ---- pre-delay gap ---------------------------------------------------
    const preSec = params.predelayMs / 1000
    if (preSec > 0) {
      ctx.fillStyle = alpha(faint, 0.09)
      ctx.fillRect(padL, padT, Math.max(x(preSec) - padL, 0), h)
      const gx = Math.round(x(preSec)) + 0.5
      ctx.strokeStyle = alpha(faint, 0.5)
      ctx.setLineDash([2, 3])
      ctx.beginPath()
      ctx.moveTo(gx, padT)
      ctx.lineTo(gx, padT + h)
      ctx.stroke()
      ctx.setLineDash([])
    }

    // ---- band envelopes --------------------------------------------------
    // Sampled once per band, then turned into both a stroke and (for the mid
    // band) a closed region. Building the region by appending the stroke path
    // would leave two subpaths, and canvas closes each one on its own — which
    // fills a spurious wedge back to the start point.
    const envelope = (rt60: number): [number, number][] => {
      const points: [number, number][] = []
      const steps = Math.max(Math.round(w), 2)
      for (let i = 0; i <= steps; i++) {
        const t = (i / steps) * windowSec
        if (t < preSec) continue
        const db = levelDbAt(t, preSec, rt60)
        if (db < FLOOR_DB) {
          points.push([x(t), y(FLOOR_DB)])
          break
        }
        points.push([x(t), y(db)])
      }
      return points
    }

    const stroke = (points: [number, number][]) => {
      const path = new Path2D()
      for (const [index, [px, py]] of points.entries()) {
        if (index === 0) path.moveTo(px, py)
        else path.lineTo(px, py)
      }
      return path
    }

    const tailColor = params.freeze ? warn : accent
    const tailBright = params.freeze ? warn : accentBright
    const mid = envelope(model.midSec)

    // Filled mid band first, so the low/high edges read on top of it.
    if (mid.length > 1) {
      const first = mid[0]!
      const last = mid[mid.length - 1]!
      const region = new Path2D()
      region.moveTo(first[0], y(FLOOR_DB))
      for (const [px, py] of mid) region.lineTo(px, py)
      region.lineTo(last[0], y(FLOOR_DB))
      region.closePath()

      const fill = ctx.createLinearGradient(0, padT, 0, padT + h)
      fill.addColorStop(0, alpha(tailColor, 0.34))
      fill.addColorStop(1, alpha(tailColor, 0.02))
      ctx.fillStyle = fill
      ctx.fill(region)
    }

    // ---- early reflections ----------------------------------------------
    // Line arrivals and their first recirculations. Diffusion smears discrete
    // reflections into the tail, so it fades these out as it rises.
    const smear = 1 - diffusionGain(params.mode, params.diffusion) / 0.78
    const tickAlpha = 0.1 + smear * 0.32
    ctx.strokeStyle = alpha(tailBright, tickAlpha)
    ctx.lineWidth = 1
    for (const delayMs of model.lineDelaysMs) {
      for (let k = 1; k <= 2; k++) {
        const t = preSec + (delayMs * k) / 1000
        if (t > windowSec) break
        const db = levelDbAt(t, preSec, model.midSec)
        if (db < FLOOR_DB) break
        const gx = Math.round(x(t)) + 0.5
        ctx.beginPath()
        ctx.moveTo(gx, padT + h)
        ctx.lineTo(gx, y(db))
        ctx.stroke()
      }
    }

    // ---- band outlines ---------------------------------------------------
    ctx.lineWidth = 1
    ctx.setLineDash([3, 3])
    ctx.strokeStyle = alpha(tailColor, 0.55)
    ctx.stroke(stroke(envelope(model.lowSec)))
    ctx.stroke(stroke(envelope(model.highSec)))
    ctx.setLineDash([])

    ctx.lineWidth = 1.75
    ctx.strokeStyle = tailBright
    ctx.stroke(stroke(mid))
  }

  $effect(() => {
    // Touch every input so the redraw re-runs when any of them changes.
    void params
    void windowSec
    void cssWidth
    void cssHeight
    draw()
  })

  $effect(() => {
    if (!host) return
    const observer = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect
      if (!rect) return
      cssWidth = rect.width
      cssHeight = rect.height
    })
    observer.observe(host)
    return () => observer.disconnect()
  })

  const legend = $derived([
    { key: 'low', label: 'LOW', sec: model.lowSec },
    { key: 'mid', label: 'MID', sec: model.midSec },
    { key: 'high', label: 'HIGH', sec: model.highSec },
  ])

  function seconds(value: number): string {
    if (!Number.isFinite(value)) return '∞'
    return value >= 10 ? value.toFixed(1) : value.toFixed(2)
  }
</script>

<div class="view" bind:this={host} class:frozen={params.freeze}>
  <canvas bind:this={canvas} style="width: {cssWidth}px; height: {cssHeight}px"
  ></canvas>
  <div class="legend">
    {#each legend as band (band.key)}
      <div class="band">
        <span class="band-label">{band.label}</span>
        <span class="band-value"
          >{seconds(band.sec)}<span class="s">s</span></span
        >
      </div>
    {/each}
  </div>
</div>

<style>
  .view {
    position: relative;
    flex: 1;
    min-height: 200px;
    background: linear-gradient(180deg, var(--stage-top), var(--stage-bottom));
    overflow: hidden;
  }

  .view.frozen {
    box-shadow: inset 0 0 0 1px var(--warn-dim);
  }

  canvas {
    display: block;
  }

  .legend {
    position: absolute;
    top: 0.65rem;
    right: 0.75rem;
    display: flex;
    gap: 1rem;
    padding: 0.4rem 0.7rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--overlay-scrim);
    pointer-events: none;
  }

  .band {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 0.1rem;
  }

  .band-label {
    font-size: 0.65rem;
    font-weight: 650;
    letter-spacing: 0.08em;
    color: var(--text-faint);
  }

  .band-value {
    font-size: 0.85rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--text);
  }

  .s {
    font-size: 0.65rem;
    color: var(--text-faint);
    margin-left: 1px;
  }
</style>
