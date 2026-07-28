<script lang="ts">
  import {
    STAGE_TICKS_DB,
    grNorm,
    levelNorm,
    type HistorySample,
  } from '../meter'
  import {
    createWaveRenderer,
    type WaveRenderer,
  } from './waveRenderer'

  type PeakTag = {
    x: number
    y: number
    label: string
  }

  type Props = {
    history: HistorySample[]
    ceilingDb: number
    live: boolean
    tags?: PeakTag[]
  }

  const { history, ceilingDb, live, tags = [] }: Props = $props()

  let gpuCanvas: HTMLCanvasElement | undefined = $state()
  let fallbackCanvas: HTMLCanvasElement | undefined = $state()
  let renderer: WaveRenderer | null = $state(null)
  let webGpuUnavailable = $state(false)
  let width = $state(0)
  let height = $state(0)

  $effect(() => {
    const el = gpuCanvas
    if (!el) return
    const parent = el.parentElement
    if (!parent) return
    const ro = new ResizeObserver((entries) => {
      const box = entries[0]?.contentRect
      if (!box) return
      width = Math.max(1, Math.floor(box.width * devicePixelRatio))
      height = Math.max(1, Math.floor(box.height * devicePixelRatio))
    })
    ro.observe(parent)
    return () => ro.disconnect()
  })

  $effect(() => {
    const el = gpuCanvas
    if (!el) return
    let cancelled = false
    createWaveRenderer(el)
      .then((next) => {
        if (cancelled) {
          next.destroy()
          return
        }
        renderer = next
        webGpuUnavailable = false
      })
      .catch(() => {
        if (!cancelled) webGpuUnavailable = true
      })
    return () => {
      cancelled = true
      renderer?.destroy()
      renderer = null
    }
  })

  $effect(() => {
    const current = renderer
    if (!current || width === 0 || height === 0) return
    current.resize(width, height)
    void history
    void ceilingDb
    void live
    current.render(history, ceilingDb, live)
  })

  $effect(() => {
    const el = fallbackCanvas
    if (
      !webGpuUnavailable ||
      !el ||
      width === 0 ||
      height === 0
    ) return
    el.width = width
    el.height = height
    const ctx = el.getContext('2d')
    if (!ctx) return
    draw(ctx, width, height, history, ceilingDb, live)
  })

  function draw(
    ctx: CanvasRenderingContext2D,
    w: number,
    h: number,
    samples: HistorySample[],
    ceiling: number,
    isLive: boolean,
  ) {
    ctx.clearRect(0, 0, w, h)
    ctx.fillStyle = '#060708'
    ctx.fillRect(0, 0, w, h)

    // Sparse horizontal rules
    ctx.strokeStyle = 'rgba(255,255,255,0.035)'
    ctx.lineWidth = 1
    for (const tick of STAGE_TICKS_DB) {
      if (tick === 0) continue
      const y = levelNorm(tick) * h
      ctx.beginPath()
      ctx.moveTo(0, y)
      ctx.lineTo(w, y)
      ctx.stroke()
    }

    // 0 dB ceiling of the face
    ctx.strokeStyle = 'rgba(255,255,255,0.1)'
    ctx.beginPath()
    ctx.moveTo(0, 0.5)
    ctx.lineTo(w, 0.5)
    ctx.stroke()

    // Output ceiling guide
    const ceilY = levelNorm(ceiling) * h
    ctx.strokeStyle = 'rgba(232, 196, 90, 0.65)'
    ctx.lineWidth = 1
    ctx.setLineDash([5 * devicePixelRatio, 4 * devicePixelRatio])
    ctx.beginPath()
    ctx.moveTo(0, ceilY)
    ctx.lineTo(w, ceilY)
    ctx.stroke()
    ctx.setLineDash([])

    if (samples.length < 2) return
    const col = w / Math.max(samples.length - 1, 1)

    // Steel input body
    ctx.beginPath()
    ctx.moveTo(0, h)
    for (let i = 0; i < samples.length; i++) {
      ctx.lineTo(i * col, levelNorm(isLive ? samples[i]!.inDb : -48) * h)
    }
    ctx.lineTo((samples.length - 1) * col, h)
    ctx.closePath()
    const body = ctx.createLinearGradient(0, 0, 0, h)
    body.addColorStop(0, 'rgba(160, 200, 220, 0.55)')
    body.addColorStop(0.45, 'rgba(90, 130, 155, 0.28)')
    body.addColorStop(1, 'rgba(30, 50, 65, 0.1)')
    ctx.fillStyle = body
    ctx.fill()

    // RMS contour
    ctx.beginPath()
    for (let i = 0; i < samples.length; i++) {
      const y = levelNorm(isLive ? samples[i]!.rmsDb : -48) * h
      if (i === 0) ctx.moveTo(i * col, y)
      else ctx.lineTo(i * col, y)
    }
    ctx.strokeStyle = 'rgba(232, 196, 90, 0.7)'
    ctx.lineWidth = 1.15 * devicePixelRatio
    ctx.stroke()

    // Vermillion GR from top — the signature
    ctx.beginPath()
    ctx.moveTo(0, 0)
    for (let i = 0; i < samples.length; i++) {
      ctx.lineTo(i * col, grNorm(isLive ? samples[i]!.grDb : 0) * h * 0.52)
    }
    ctx.lineTo((samples.length - 1) * col, 0)
    ctx.closePath()
    ctx.fillStyle = 'rgba(232, 72, 58, 0.72)'
    ctx.fill()

    ctx.beginPath()
    for (let i = 0; i < samples.length; i++) {
      const y = grNorm(isLive ? samples[i]!.grDb : 0) * h * 0.52
      if (i === 0) ctx.moveTo(i * col, y)
      else ctx.lineTo(i * col, y)
    }
    ctx.strokeStyle = '#ff6a5c'
    ctx.lineWidth = 1.5 * devicePixelRatio
    ctx.stroke()
  }
</script>

<div class="wave" class:dark={!live}>
  <canvas
    class="gpu"
    class:hidden={webGpuUnavailable}
    bind:this={gpuCanvas}
    aria-hidden="true"
  ></canvas>
  <canvas
    class="fallback"
    class:visible={webGpuUnavailable}
    bind:this={fallbackCanvas}
    aria-hidden="true"
  ></canvas>
  <div class="scale" aria-hidden="true">
    {#each STAGE_TICKS_DB as tick}
      <span style="top: {(levelNorm(tick) * 100).toFixed(2)}%">{tick}</span>
    {/each}
  </div>
  {#each tags as tag (tag.x + tag.label)}
    <div
      class="tag"
      style="left: {(tag.x * 100).toFixed(2)}%; top: {(tag.y * 100).toFixed(2)}%"
    >
      {tag.label}
    </div>
  {/each}
</div>

<style>
  .wave {
    position: relative;
    overflow: hidden;
    width: 100%;
    height: 100%;
    background: var(--stage);
  }

  .wave.dark {
    opacity: 0.5;
  }

  canvas {
    position: absolute;
    inset: 0;
    display: block;
    width: 100%;
    height: 100%;
  }

  canvas.hidden {
    visibility: hidden;
  }

  canvas.fallback {
    visibility: hidden;
  }

  canvas.fallback.visible {
    visibility: visible;
  }

  .scale {
    position: absolute;
    inset: 0.4rem 0.55rem 0.4rem auto;
    width: 1.9rem;
    pointer-events: none;
  }

  .scale span {
    position: absolute;
    right: 0;
    transform: translateY(-50%);
    color: rgba(200, 210, 225, 0.38);
    font-size: 0.55rem;
    font-weight: 600;
  }

  .tag {
    position: absolute;
    z-index: 2;
    transform: translate(-50%, -120%);
    padding: 0.14rem 0.42rem;
    border-radius: 999px;
    background: var(--amber);
    color: #1a1608;
    font-size: 0.58rem;
    font-weight: 700;
    letter-spacing: 0.01em;
    pointer-events: none;
    white-space: nowrap;
    animation: tag-arrive 180ms cubic-bezier(0.2, 0.8, 0.2, 1);
  }

  @keyframes tag-arrive {
    from {
      opacity: 0;
      transform: translate(-50%, -80%) scale(0.92);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .tag {
      animation: none;
    }
  }
</style>
