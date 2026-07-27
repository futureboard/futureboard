<script lang="ts">
  import { untrack } from 'svelte'

  import {
    advance,
    angleFor,
    normFor,
    ticksFor,
    type MeterMode,
  } from '../meter'

  type Props = {
    mode: MeterMode
    /** dB of reduction, or dB relative to 0 VU, depending on `mode`. */
    value: number
    /** No telemetry yet: the needle parks and the face says so. */
    live: boolean
    clip: boolean
    onclearclip: () => void
  }

  const { mode, value, live, clip, onclearclip }: Props = $props()

  const PIVOT_X = 100
  const PIVOT_Y = 134
  const NEEDLE_R = 100
  const SCALE_R = 104
  const LABEL_R = 86
  const TICK_MAJOR = 9
  const TICK_MINOR = 5

  const ticks = $derived(ticksFor(mode))
  const target = $derived(
    live
      ? normFor(mode, value)
      : normFor(mode, mode === 'reduction' ? 0 : -20),
  )

  let needle = $state(untrack(() => target))

  $effect(() => {
    let raf = 0
    let last = performance.now()
    const step = (now: number) => {
      const dt = Math.min((now - last) / 1000, 0.25)
      last = now
      needle = advance(needle, target, dt)
      raf = requestAnimationFrame(step)
    }
    raf = requestAnimationFrame(step)
    return () => cancelAnimationFrame(raf)
  })

  function polar(norm: number, radius: number) {
    const rad = (angleFor(norm) * Math.PI) / 180
    return {
      x: PIVOT_X + Math.sin(rad) * radius,
      y: PIVOT_Y - Math.cos(rad) * radius,
    }
  }

  const needleTip = $derived(polar(needle, NEEDLE_R))
  const arcStart = polar(0, SCALE_R)
  const arcEnd = polar(1, SCALE_R)
</script>

<div class="meter" class:dark={!live}>
  <svg
    viewBox="0 0 200 120"
    role="img"
    aria-label="{mode === 'reduction' ? 'Gain reduction' : 'Output level'} meter"
  >
    <defs>
      <linearGradient id="face" x1="0" x2="0" y1="0" y2="1">
        <stop offset="0" stop-color="#3a72ad" />
        <stop offset="0.55" stop-color="#2a5f9a" />
        <stop offset="1" stop-color="#1a4574" />
      </linearGradient>
    </defs>

    <rect x="0" y="0" width="200" height="120" rx="2" fill="url(#face)" />

    <path
      class="arc"
      d="M {arcStart.x.toFixed(2)} {arcStart.y.toFixed(2)} A {SCALE_R} {SCALE_R} 0 0 1 {arcEnd.x.toFixed(2)} {arcEnd.y.toFixed(2)}"
    />

    {#each [ticks.filter((t) => t.hot)] as hot}
      {#if hot.length > 1}
        {@const a = polar(Math.min(...hot.map((t) => t.norm)), SCALE_R)}
        {@const b = polar(Math.max(...hot.map((t) => t.norm)), SCALE_R)}
        <path
          class="arc hot"
          d="M {a.x.toFixed(2)} {a.y.toFixed(2)} A {SCALE_R} {SCALE_R} 0 0 1 {b.x.toFixed(2)} {b.y.toFixed(2)}"
        />
      {/if}
    {/each}

    {#each ticks as tick (tick.norm)}
      {@const outer = polar(tick.norm, SCALE_R)}
      {@const inner = polar(
        tick.norm,
        SCALE_R - (tick.major ? TICK_MAJOR : TICK_MINOR),
      )}
      {@const label = polar(tick.norm, LABEL_R)}
      <line
        class="tick"
        class:major={tick.major}
        class:hot={tick.hot}
        x1={outer.x}
        y1={outer.y}
        x2={inner.x}
        y2={inner.y}
      />
      {#if tick.label}
        <text
          class="label"
          class:hot={tick.hot}
          x={label.x}
          y={label.y}
        >{tick.label}</text>
      {/if}
    {/each}

    <line
      class="needle"
      x1={PIVOT_X}
      y1={PIVOT_Y}
      x2={needleTip.x}
      y2={needleTip.y}
    />
    <circle class="hub" cx={PIVOT_X} cy={PIVOT_Y} r="4.5" />

    <text class="caption" x="100" y="28">
      {mode === 'reduction' ? 'GAIN REDUCTION' : 'OUTPUT'}
    </text>
  </svg>

  <button
    type="button"
    class="clip"
    class:on={clip}
    aria-label="Clear clip"
    title="Clear clip"
    onclick={onclearclip}
  ></button>
</div>

<style>
  .meter {
    position: relative;
    width: 100%;
    max-width: 22rem;
  }

  .meter.dark {
    opacity: 0.72;
  }

  svg {
    display: block;
    width: 100%;
    height: auto;
    border-radius: 2px;
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.08),
      0 0 0 1px rgba(0, 0, 0, 0.55);
  }

  .arc {
    fill: none;
    stroke: rgba(240, 244, 248, 0.55);
    stroke-width: 1.4;
  }

  .arc.hot {
    stroke: var(--meter-hot);
    stroke-width: 2.2;
  }

  .tick {
    stroke: rgba(240, 244, 248, 0.65);
    stroke-width: 1;
    stroke-linecap: round;
  }

  .tick.major {
    stroke-width: 1.6;
  }

  .tick.hot,
  .label.hot {
    stroke: var(--meter-hot);
    fill: var(--meter-hot);
  }

  .label {
    fill: var(--meter-ink);
    font-size: 7.5px;
    font-weight: 650;
    text-anchor: middle;
    dominant-baseline: middle;
  }

  .needle {
    stroke: #f5f2ec;
    stroke-width: 1.6;
    stroke-linecap: round;
    filter: drop-shadow(0 1px 1px rgba(0, 0, 0, 0.45));
  }

  .hub {
    fill: #d8dde4;
    stroke: #1a2030;
    stroke-width: 1;
  }

  .caption {
    fill: rgba(240, 244, 248, 0.72);
    font-size: 7px;
    font-weight: 700;
    letter-spacing: 0.18em;
    text-anchor: middle;
  }

  .clip {
    position: absolute;
    top: 0.45rem;
    right: 0.45rem;
    width: 0.55rem;
    height: 0.55rem;
    border-radius: 50%;
    background: #2a3140;
    box-shadow: inset 0 1px 2px rgba(0, 0, 0, 0.55);
  }

  .clip.on {
    background: var(--clip);
    box-shadow: 0 0 8px var(--clip);
  }
</style>
