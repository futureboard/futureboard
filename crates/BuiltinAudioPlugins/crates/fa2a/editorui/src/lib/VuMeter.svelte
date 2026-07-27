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
        <stop offset="0" stop-color="#fff7e5" />
        <stop offset="0.55" stop-color="#f5e7c9" />
        <stop offset="1" stop-color="#e7d2aa" />
      </linearGradient>
    </defs>

    <rect x="0" y="0" width="200" height="120" rx="3" fill="url(#face)" />

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
      {@const text = polar(tick.norm, LABEL_R)}
      <line
        class="tick"
        class:hot={tick.hot}
        class:major={tick.major}
        x1={outer.x}
        y1={outer.y}
        x2={inner.x}
        y2={inner.y}
      />
      {#if tick.label}
        <text class="label" class:hot={tick.hot} x={text.x} y={text.y}>
          {tick.label}
        </text>
      {/if}
    {/each}

    <text class="unit" x="100" y="86">
      {mode === 'reduction' ? 'GAIN REDUCTION dB' : 'VU'}
    </text>

    <g class="needle" class:parked={!live}>
      <line x1={PIVOT_X} y1={PIVOT_Y} x2={needleTip.x} y2={needleTip.y} />
      <circle cx={PIVOT_X} cy={PIVOT_Y} r="6.5" />
    </g>

    <rect class="bezel" x="0.8" y="0.8" width="198.4" height="118.4" rx="2.5" />
  </svg>

  {#if clip}
    <button
      type="button"
      class="clip"
      onclick={onclearclip}
      title="Output clipped — click to reset"
    >
      CLIP
    </button>
  {/if}

  {#if !live}
    <div class="offline">no signal</div>
  {/if}
</div>

<style>
  .meter {
    position: relative;
    width: 100%;
  }

  svg {
    display: block;
    width: 100%;
    height: auto;
    border-radius: 2px;
    box-shadow: inset 0 1px 3px rgba(70, 48, 18, 0.14);
  }

  .arc {
    fill: none;
    stroke: var(--meter-ink);
    stroke-width: 1.35;
  }

  .arc.hot {
    stroke: var(--meter-hot);
    stroke-width: 2.4;
  }

  .tick {
    stroke: var(--meter-ink);
    stroke-width: 1.05;
    stroke-linecap: round;
  }

  .tick.major {
    stroke-width: 1.8;
  }

  .tick.hot {
    stroke: var(--meter-hot);
  }

  .label {
    fill: var(--meter-ink);
    font-family: 'Helvetica Neue', Helvetica, Arial, sans-serif;
    font-size: 8.2px;
    font-weight: 600;
    text-anchor: middle;
    dominant-baseline: middle;
  }

  .label.hot {
    fill: var(--meter-hot);
  }

  .unit {
    fill: #6b5840;
    font-family: 'Helvetica Neue', Helvetica, Arial, sans-serif;
    font-size: 6.8px;
    font-weight: 700;
    letter-spacing: 0.14em;
    text-anchor: middle;
  }

  .needle line {
    stroke: #160f08;
    stroke-width: 1.85;
    stroke-linecap: round;
  }

  .needle circle {
    fill: #160f08;
  }

  .needle.parked line {
    stroke: #8a7860;
  }

  .bezel {
    fill: none;
    stroke: rgba(35, 26, 14, 0.32);
    stroke-width: 1.4;
  }

  .clip {
    position: absolute;
    top: 8%;
    right: 5.5%;
    padding: 0.15rem 0.4rem;
    border-radius: 2px;
    background: var(--clip);
    color: #fff;
    font-size: 0.55rem;
    font-weight: 750;
    letter-spacing: 0.1em;
    cursor: pointer;
  }

  .offline {
    position: absolute;
    left: 50%;
    bottom: 5%;
    transform: translateX(-50%);
    color: #8a7860;
    font-size: 0.55rem;
    font-weight: 650;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }
</style>
