<script lang="ts">
  import type { EchoParams } from '../bridge'
  import { echoModel, envelopeAt, filterMagnitude, tapTimes } from '../model'

  type Props = { params: EchoParams }
  const { params }: Props = $props()

  let canvas: HTMLCanvasElement | undefined = $state()
  let host: HTMLDivElement | undefined = $state()
  let cssWidth = $state(0)
  let cssHeight = $state(0)

  /** Longest window the axis will show, so a 4 s delay does not squash the
   *  first repeat into the left edge. */
  const MAX_WINDOW_SEC = 6

  /** Reference tone the per-pass filter dulling is measured at. Sits inside the
   *  high cut's usual range so the colour actually tracks it, but low enough
   *  that a dark setting does not put every repeat past the floor at once. */
  const TONE_REF_HZ = 3000

  /** Ceiling on how far a repeat's colour is pulled toward grey. Fully grey
   *  would lose which channel the repeat is on, which is the display's whole
   *  point under ping-pong. */
  const MAX_DULL = 0.6

  /** Bottom of the amplitude axis, in decibels. Matches the -60 the repeat
   *  model stops at. */
  const FLOOR_DB = -60

  const model = $derived(echoModel(params))
  const times = $derived(tapTimes(params))

  /** Repeats below this are drawn but do not get to widen the time axis — at a
   *  modest feedback the last few are 50 dB down and would leave most of the
   *  plot empty. */
  const WINDOW_FLOOR_DB = -40

  const windowSec = $derived.by(() => {
    const floor = 10 ** (WINDOW_FLOOR_DB / 20)
    const audible = model.taps.filter((tap) => tap.amplitude >= floor)
    const last = audible.length > 0 ? audible[audible.length - 1]!.time : 0
    return Math.min(MAX_WINDOW_SEC, Math.max(0.25, last * 1.15))
  })

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

  /** Blend two `#rrggbb` colours, `t` = 0 keeps `from`. */
  function blend(from: string, to: string, t: number, a: number): string {
    const parse = (hex: string) => {
      const match = /^#?([0-9a-f]{6})$/i.exec(hex.trim())
      return match?.[1] ? parseInt(match[1], 16) : null
    }
    const f = parse(from)
    const g = parse(to)
    if (f === null || g === null) return alpha(from, a)
    const mix = (shift: number) => {
      const a0 = (f >> shift) & 255
      const b0 = (g >> shift) & 255
      return Math.round(a0 + (b0 - a0) * Math.min(Math.max(t, 0), 1))
    }
    return `rgba(${mix(16)}, ${mix(8)}, ${mix(0)}, ${a})`
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

    const accent = cssVar('--accent', '#2fd6a6')
    const accentBright = cssVar('--accent-bright', '#62f2c8')
    const alt = cssVar('--accent-alt', '#4aa8ff')
    const altBright = cssVar('--accent-alt-bright', '#7cc4ff')
    const warn = cssVar('--warn', '#f0a83c')
    const grid = cssVar('--grid', '#1c2727')
    const faint = cssVar('--text-faint', '#62726f')
    const dull = cssVar('--text-faint', '#62726f')

    const padL = 30
    const padR = 14
    const padT = 16
    const padB = 20
    const w = Math.max(cssWidth - padL - padR, 1)
    const h = Math.max(cssHeight - padT - padB, 1)
    const mid = padT + h / 2
    // Each lane gets half the height, less a small gap at the centre line.
    const lane = h / 2 - 5

    const x = (t: number) => padL + (t / windowSec) * w

    // Bar height is decibels, not raw amplitude. On a linear scale everything
    // past the second repeat collapses into a stub at the centre line, even
    // though a -30 dB repeat is plainly audible.
    const height = (amplitude: number) => {
      if (amplitude <= 0) return 0
      const db = 20 * Math.log10(amplitude)
      if (db <= FLOOR_DB) return 0
      return lane * (1 - db / FLOOR_DB)
    }

    ctx.font =
      '9px ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace'
    ctx.lineWidth = 1

    // ---- time grid -------------------------------------------------------
    const tickSec =
      windowSec <= 0.5 ? 0.1 : windowSec <= 1.5 ? 0.25 : windowSec <= 4 ? 0.5 : 1
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

    // ---- amplitude grid --------------------------------------------------
    ctx.textAlign = 'right'
    ctx.textBaseline = 'middle'
    for (const db of [-12, -24, -36, -48]) {
      const off = height(10 ** (db / 20))
      for (const sign of [-1, 1]) {
        const gy = Math.round(mid + sign * off) + 0.5
        ctx.strokeStyle = grid
        ctx.beginPath()
        ctx.moveTo(padL, gy)
        ctx.lineTo(padL + w, gy)
        ctx.stroke()
      }
      // Labelled on the upper lane only — the lower one mirrors it exactly.
      ctx.fillStyle = faint
      ctx.fillText(`${db}`, padL - 6, mid - off)
    }

    // ---- lane labels + centre line --------------------------------------
    ctx.strokeStyle = alpha(faint, 0.22)
    ctx.beginPath()
    ctx.moveTo(padL, Math.round(mid) + 0.5)
    ctx.lineTo(padL + w, Math.round(mid) + 0.5)
    ctx.stroke()

    ctx.textAlign = 'left'
    ctx.textBaseline = 'middle'
    ctx.fillStyle = alpha(faint, 0.9)
    ctx.fillText('L', padL + 12, padT + 5)
    ctx.fillText('R', padL + 12, padT + h - 5)

    // ---- dry signal ------------------------------------------------------
    // Drawn at unity on both lanes so the repeats have a reference to read
    // against; it is the input, not a repeat.
    const dryX = Math.round(x(0)) + 0.5
    ctx.strokeStyle = alpha(faint, 0.85)
    ctx.lineWidth = 2
    ctx.beginPath()
    ctx.moveTo(dryX, mid - lane)
    ctx.lineTo(dryX, mid + lane)
    ctx.stroke()

    // ---- feedback envelopes ---------------------------------------------
    const floor = 10 ** (-60 / 20)
    const envelope = (step: number, sign: number) => {
      const path = new Path2D()
      const steps = Math.max(Math.round(w), 2)
      let started = false
      for (let i = 0; i <= steps; i++) {
        const t = (i / steps) * windowSec
        const a = envelopeAt(t, step, model.gain)
        if (a < floor) break
        const py = mid + sign * height(Math.min(a, 1))
        if (started) path.lineTo(x(t), py)
        else {
          path.moveTo(x(t), py)
          started = true
        }
      }
      return path
    }

    const tailColor = params.freeze ? warn : accent
    ctx.setLineDash([3, 3])
    ctx.lineWidth = 1
    ctx.strokeStyle = alpha(tailColor, 0.4)
    ctx.stroke(envelope(times.left, -1))
    ctx.stroke(envelope(times.right, 1))
    ctx.setLineDash([])

    // ---- repeats ---------------------------------------------------------
    // Each pass runs through the feedback filters once more, so successive
    // repeats are drawn duller by exactly the magnitude those filters have at
    // the reference tone.
    const perPass = filterMagnitude(params, TONE_REF_HZ)
    for (const tap of model.taps) {
      if (tap.time > windowSec) continue
      const sign = tap.lane === 'left' ? -1 : 1
      const base = tap.lane === 'left' ? accentBright : altBright
      const color = params.freeze ? warn : base
      const tone = perPass ** tap.pass
      const dulled = Math.min(1 - tone, MAX_DULL)
      const gx = Math.round(x(tap.time)) + 0.5
      const bar = height(Math.min(tap.amplitude, 1))

      // Opacity floors well above zero: a -40 dB repeat is quiet, not absent,
      // and an almost-invisible bar reads as the delay having stopped.
      ctx.strokeStyle = blend(color, dull, dulled, 0.5 + tap.amplitude * 0.45)
      ctx.lineWidth = 2.5
      ctx.beginPath()
      ctx.moveTo(gx, mid)
      ctx.lineTo(gx, mid + sign * bar)
      ctx.stroke()

      // Cap the bar so a short repeat still reads as a discrete event.
      ctx.fillStyle = blend(color, dull, dulled, 0.7 + tap.amplitude * 0.3)
      ctx.fillRect(gx - 2, mid + sign * bar - (sign < 0 ? 0 : 2), 4, 2)
    }

    // ---- tap-time markers ------------------------------------------------
    // Drawn full height rather than in "their" lane: a tap time marks a moment,
    // and under ping-pong the line's own first repeat comes back on the
    // opposite side — a half-height marker would point at the wrong one.
    for (const [step, color] of [
      [times.left, accent],
      [times.right, alt],
    ] as const) {
      if (step > windowSec) continue
      const gx = Math.round(x(step)) + 0.5
      ctx.strokeStyle = alpha(color, 0.4)
      ctx.setLineDash([1, 3])
      ctx.beginPath()
      ctx.moveTo(gx, padT)
      ctx.lineTo(gx, padT + h)
      ctx.stroke()
      ctx.setLineDash([])
    }
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

  function ms(value: number): string {
    return value >= 1 ? `${value.toFixed(2)}s` : `${Math.round(value * 1000)}ms`
  }
</script>

<div class="view" bind:this={host} class:frozen={params.freeze}>
  <canvas bind:this={canvas} style="width: {cssWidth}px; height: {cssHeight}px"
  ></canvas>
  <div class="overlay">
    <div class="caption">
      <span class="title">Echo model</span>
      <span class="note">computed from parameters · repeats over time</span>
    </div>
    <div class="legend">
      <div class="item">
        <span class="key">L</span>
        <span class="val">{ms(times.left)}</span>
      </div>
      <div class="item">
        <span class="key">R</span>
        <span class="val">{ms(times.right)}</span>
      </div>
      <div class="item">
        <span class="key">Repeats</span>
        <span class="val">{params.freeze ? '∞' : model.passes}</span>
      </div>
    </div>
  </div>
</div>

<style>
  .view {
    position: relative;
    flex: 1;
    min-height: 0;
    background: linear-gradient(180deg, var(--stage-top), var(--stage-bottom));
    overflow: hidden;
  }

  .view.frozen {
    box-shadow: inset 0 0 0 1px var(--warn-dim);
  }

  canvas {
    display: block;
  }

  .overlay {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }

  .caption {
    position: absolute;
    left: 2rem;
    bottom: 1.4rem;
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 0.2rem 0.4rem;
    border-radius: var(--radius-sm);
    background: var(--overlay-scrim);
  }

  .title {
    color: var(--text-muted);
    font-size: 0.6rem;
    font-weight: 650;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }

  .note {
    color: var(--text-faint);
    font-size: 0.6rem;
  }

  .legend {
    position: absolute;
    top: 0.5rem;
    right: 0.6rem;
    display: flex;
    gap: 0.75rem;
    padding: 0.2rem 0.45rem;
    border-radius: var(--radius-sm);
    background: var(--overlay-scrim);
  }

  .item {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 1px;
  }

  .key {
    color: var(--text-faint);
    font-size: 0.55rem;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }

  .val {
    color: var(--text);
    font-size: 0.72rem;
    font-weight: 600;
  }
</style>
