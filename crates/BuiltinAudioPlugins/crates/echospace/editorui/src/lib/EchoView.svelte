<script lang="ts">
  import type { EchoParams } from '../bridge'
  import { echoModel, envelopeAt, filterMagnitude, tapTimes } from '../model'
  import { MODE_HINTS } from '../params'

  type Props = { params: EchoParams }
  const { params }: Props = $props()

  let canvas: HTMLCanvasElement | undefined = $state()
  let host: HTMLDivElement | undefined = $state()
  let cssWidth = $state(0)
  let cssHeight = $state(0)

  const MAX_WINDOW_SEC = 6
  const TONE_REF_HZ = 3000
  const MAX_DULL = 0.5
  const FLOOR_DB = -60
  const WINDOW_FLOOR_DB = -40
  /** Number echo marks 1..N so the first hits read as a countable sequence. */
  const NUMBERED_PASSES = 4

  const model = $derived(echoModel(params))
  const times = $derived(tapTimes(params))

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

  function alpha(hex: string, a: number): string {
    const match = /^#?([0-9a-f]{6})$/i.exec(hex.trim())
    if (!match?.[1]) return hex
    const int = parseInt(match[1], 16)
    return `rgba(${(int >> 16) & 255}, ${(int >> 8) & 255}, ${int & 255}, ${a})`
  }

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
    const text = cssVar('--text', '#e6f0ee')

    const padL = 44
    const padR = 16
    const padT = 30
    const padB = 30
    const w = Math.max(cssWidth - padL - padR, 1)
    const h = Math.max(cssHeight - padT - padB, 1)
    const base = padT + h

    const x = (t: number) => padL + (t / windowSec) * w
    const barH = (amplitude: number) => {
      if (amplitude <= 0) return 0
      const db = 20 * Math.log10(amplitude)
      if (db <= FLOOR_DB) return 0
      return h * (1 - db / FLOOR_DB)
    }

    // ---- light time grid (no dB numbers — those read as engineering) -----
    const tickSec =
      windowSec <= 0.5 ? 0.1 : windowSec <= 1.5 ? 0.25 : windowSec <= 4 ? 0.5 : 1
    ctx.lineWidth = 1
    ctx.font =
      '10px Inter, "Segoe UI", system-ui, sans-serif'
    ctx.textAlign = 'center'
    ctx.textBaseline = 'top'
    for (let t = tickSec; t < windowSec; t += tickSec) {
      const gx = Math.round(x(t)) + 0.5
      ctx.strokeStyle = grid
      ctx.beginPath()
      ctx.moveTo(gx, padT)
      ctx.lineTo(gx, base)
      ctx.stroke()
      ctx.fillStyle = faint
      ctx.fillText(
        tickSec < 1 ? `${Math.round(t * 1000)} ms` : `${t.toFixed(0)} s`,
        gx,
        base + 8,
      )
    }

    // Soft loudness guides without numbers.
    for (const db of [-18, -36]) {
      const gy = Math.round(base - barH(10 ** (db / 20))) + 0.5
      ctx.strokeStyle = alpha(faint, 0.12)
      ctx.beginPath()
      ctx.moveTo(padL, gy)
      ctx.lineTo(padL + w, gy)
      ctx.stroke()
    }

    ctx.save()
    ctx.translate(14, padT + h / 2)
    ctx.rotate(-Math.PI / 2)
    ctx.textAlign = 'center'
    ctx.textBaseline = 'middle'
    ctx.fillStyle = faint
    ctx.font = '650 9px Inter, "Segoe UI", system-ui, sans-serif'
    ctx.fillText('quiet  →  loud', 0, 0)
    ctx.restore()

    ctx.strokeStyle = alpha(faint, 0.4)
    ctx.beginPath()
    ctx.moveTo(padL, Math.round(base) + 0.5)
    ctx.lineTo(padL + w, Math.round(base) + 0.5)
    ctx.stroke()

    // ---- soft decay fill (shows “getting quieter”) -----------------------
    const floor = 10 ** (FLOOR_DB / 20)
    const fillEnvelope = (step: number, color: string) => {
      const region = new Path2D()
      const steps = Math.max(Math.round(w), 2)
      let started = false
      let lastX = padL
      for (let i = 0; i <= steps; i++) {
        const t = (i / steps) * windowSec
        const a = envelopeAt(t, step, model.gain)
        if (a < floor) break
        const px = x(t)
        const py = base - barH(Math.min(a, 1))
        lastX = px
        if (started) region.lineTo(px, py)
        else {
          region.moveTo(px, base)
          region.lineTo(px, py)
          started = true
        }
      }
      if (!started) return
      region.lineTo(lastX, base)
      region.closePath()
      ctx.fillStyle = alpha(color, 0.07)
      ctx.fill(region)
    }

    if (params.freeze) {
      fillEnvelope(times.left, warn)
    } else {
      fillEnvelope(times.left, accent)
      if (Math.abs(times.right - times.left) > 0.0005) {
        fillEnvelope(times.right, alt)
      }
    }

    // ---- original sound --------------------------------------------------
    const dryX = Math.round(x(0)) + 0.5
    ctx.fillStyle = alpha(text, 0.16)
    ctx.fillRect(dryX - 4, padT + 8, 8, h - 8)
    ctx.fillStyle = alpha(text, 0.85)
    ctx.beginPath()
    ctx.roundRect(dryX - 5, padT + 4, 10, 10, 2)
    ctx.fill()
    ctx.fillStyle = faint
    ctx.font = '650 9px Inter, "Segoe UI", system-ui, sans-serif'
    ctx.textAlign = 'center'
    ctx.textBaseline = 'bottom'
    ctx.fillText('Sound', dryX, padT - 4)

    // ---- echoes ----------------------------------------------------------
    const perPass = filterMagnitude(params, TONE_REF_HZ)
    const occupied = new Map<number, number>()
    for (const tap of model.taps) {
      if (tap.time > windowSec) continue
      const key = Math.round(tap.time * 1000)
      const peers = occupied.get(key) ?? 0
      occupied.set(key, peers + 1)

      const baseColor =
        tap.lane === 'left'
          ? params.freeze
            ? warn
            : accentBright
          : params.freeze
            ? warn
            : altBright
      const tone = perPass ** tap.pass
      const dulled = Math.min(1 - tone, MAX_DULL)
      const nudge =
        peers > 0
          ? tap.lane === 'left'
            ? -3
            : 3
          : tap.lane === 'left'
            ? -1.5
            : 1.5
      const gx = Math.round(x(tap.time) + nudge) + 0.5
      const top = base - barH(Math.min(tap.amplitude, 1))

      ctx.strokeStyle = blend(baseColor, dull, dulled, 0.55 + tap.amplitude * 0.4)
      ctx.lineWidth = 6
      ctx.lineCap = 'round'
      ctx.beginPath()
      ctx.moveTo(gx, base - 1)
      ctx.lineTo(gx, top + 3)
      ctx.stroke()

      ctx.fillStyle = blend(baseColor, dull, dulled, 0.9)
      ctx.beginPath()
      ctx.arc(gx, top + 1, 3, 0, Math.PI * 2)
      ctx.fill()

      if (tap.pass <= NUMBERED_PASSES && tap.amplitude > 0.08) {
        ctx.fillStyle = alpha(text, 0.75)
        ctx.font = '650 9px Inter, "Segoe UI", system-ui, sans-serif'
        ctx.textAlign = 'center'
        ctx.textBaseline = 'bottom'
        ctx.fillText(String(tap.pass), gx, top - 4)
      }
    }
  }

  $effect(() => {
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

  const mono = $derived(params.mode === 'mono')
  const story = $derived(MODE_HINTS[params.mode])
</script>

<div class="view" bind:this={host} class:frozen={params.freeze}>
  <canvas bind:this={canvas} style="width: {cssWidth}px; height: {cssHeight}px"
  ></canvas>
  <div class="overlay">
    <div class="caption">
      <span class="title">Each bar is one echo</span>
      <span class="note">{story}</span>
    </div>
    <div class="legend">
      <div class="item">
        <span class="swatch left"></span>
        <div class="copy">
          <span class="key">{mono ? 'Delay' : 'Left'}</span>
          <span class="val">{ms(times.left)}</span>
        </div>
      </div>
      {#if !mono}
        <div class="item">
          <span class="swatch right"></span>
          <div class="copy">
            <span class="key">Right</span>
            <span class="val">{ms(times.right)}</span>
          </div>
        </div>
      {/if}
      <div class="item">
        <div class="copy">
          <span class="key">Heard</span>
          <span class="val">{params.freeze ? '∞' : model.passes}</span>
        </div>
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
    left: 2.6rem;
    bottom: 1.7rem;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    max-width: min(22rem, 55%);
    padding: 0.35rem 0.55rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--overlay-scrim);
  }

  .title {
    color: var(--text);
    font-size: 0.72rem;
    font-weight: 650;
  }

  .note {
    color: var(--text-muted);
    font-size: 0.62rem;
    line-height: 1.25;
  }

  .legend {
    position: absolute;
    top: 0.55rem;
    right: 0.65rem;
    display: flex;
    gap: 0.85rem;
    padding: 0.35rem 0.55rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--overlay-scrim);
  }

  .item {
    display: flex;
    align-items: center;
    gap: 0.35rem;
  }

  .swatch {
    width: 0.45rem;
    height: 0.45rem;
    border-radius: 50%;
  }

  .swatch.left {
    background: var(--accent-bright);
  }

  .swatch.right {
    background: var(--accent-alt-bright);
  }

  .copy {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
  }

  .key {
    color: var(--text-faint);
    font-size: 0.55rem;
    font-weight: 650;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .val {
    color: var(--text);
    font-size: 0.78rem;
    font-weight: 600;
  }
</style>
