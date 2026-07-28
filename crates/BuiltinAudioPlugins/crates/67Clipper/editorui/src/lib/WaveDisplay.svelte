<script lang="ts">
  import {
    STAGE_TICKS_DB,
    grNorm,
    levelNorm,
    type HistorySample,
  } from '../meter'

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

  let canvas: HTMLCanvasElement | undefined = $state()
  let width = $state(0)
  let height = $state(0)

  $effect(() => {
    const el = canvas
    if (!el) return
    const parent = el.parentElement
    if (!parent) return
    const ro = new ResizeObserver((entries) => {
      const box = entries[0]?.contentRect
      if (!box) return
      width = Math.max(1, Math.floor(box.width * devicePixelRatio))
      height = Math.max(1, Math.floor(box.height * devicePixelRatio))
      el.width = width
      el.height = height
    })
    ro.observe(parent)
    return () => ro.disconnect()
  })

  $effect(() => {
    const el = canvas
    if (!el || width === 0 || height === 0) return
    void history
    void ceilingDb
    void live
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
    ctx.fillStyle = '#060809'
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
    ctx.strokeStyle = 'rgba(74, 163, 255, 0.55)'
    ctx.lineWidth = 1
    ctx.setLineDash([5 * devicePixelRatio, 4 * devicePixelRatio])
    ctx.beginPath()
    ctx.moveTo(0, ceilY)
    ctx.lineTo(w, ceilY)
    ctx.stroke()
    ctx.setLineDash([])

    if (samples.length < 2) return
    const col = w / Math.max(samples.length - 1, 1)

    // Muted blue input body
    ctx.beginPath()
    ctx.moveTo(0, h)
    for (let i = 0; i < samples.length; i++) {
      ctx.lineTo(i * col, levelNorm(isLive ? samples[i]!.inDb : -48) * h)
    }
    ctx.lineTo((samples.length - 1) * col, h)
    ctx.closePath()
    const body = ctx.createLinearGradient(0, 0, 0, h)
    body.addColorStop(0, 'rgba(120, 175, 230, 0.5)')
    body.addColorStop(0.45, 'rgba(61, 110, 158, 0.26)')
    body.addColorStop(1, 'rgba(20, 40, 62, 0.08)')
    ctx.fillStyle = body
    ctx.fill()

    // RMS contour
    ctx.beginPath()
    for (let i = 0; i < samples.length; i++) {
      const y = levelNorm(isLive ? samples[i]!.rmsDb : -48) * h
      if (i === 0) ctx.moveTo(i * col, y)
      else ctx.lineTo(i * col, y)
    }
    ctx.strokeStyle = 'rgba(74, 163, 255, 0.75)'
    ctx.lineWidth = 1.15 * devicePixelRatio
    ctx.stroke()

    // Red GR / clip activity from the top — the signature
    ctx.beginPath()
    ctx.moveTo(0, 0)
    for (let i = 0; i < samples.length; i++) {
      ctx.lineTo(i * col, grNorm(isLive ? samples[i]!.grDb : 0) * h * 0.52)
    }
    ctx.lineTo((samples.length - 1) * col, 0)
    ctx.closePath()
    ctx.fillStyle = 'rgba(232, 72, 58, 0.7)'
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
  <canvas bind:this={canvas} aria-hidden="true"></canvas>
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
    display: block;
    width: 100%;
    height: 100%;
  }

  .scale {
    position: absolute;
    inset: 0.4rem 0.6rem 0.4rem auto;
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
    background: var(--red);
    color: #180a08;
    font-size: 0.58rem;
    font-weight: 700;
    letter-spacing: 0.01em;
    pointer-events: none;
    white-space: nowrap;
  }
</style>
