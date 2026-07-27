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

  // The pivot sits below the visible window, as it does in the real movement.
  // The needle stops just inside the scale arc rather than crossing it.
  const PIVOT_X = 100
  const PIVOT_Y = 134
  const NEEDLE_R = 100
  const SCALE_R = 104
  const LABEL_R = 86
  const TICK_MAJOR = 9
  const TICK_MINOR = 5

  const ticks = $derived(ticksFor(mode))
  const target = $derived(live ? normFor(mode, value) : normFor(mode, mode === 'reduction' ? 0 : -20))

  // The needle is animated rather than bound straight to `target`: the host
  // sends telemetry at ~30 Hz, and a VU needle that teleports between frames
  // reads as a bar graph. `advance` uses the real 300 ms VU ballistic.
  //
  // Seeded untracked on purpose — this is the needle's starting position, not
  // a subscription; the loop below is what follows `target` afterwards.
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
  <svg viewBox="0 0 200 120" role="img" aria-label="{mode === 'reduction' ? 'Gain reduction' : 'Output level'} meter">
    <defs>
      <linearGradient id="face" x1="0" x2="0" y1="0" y2="1">
        <stop offset="0" stop-color="#f6ecd6" />
        <stop offset="0.55" stop-color="#efe0c2" />
        <stop offset="1" stop-color="#e2cea6" />
      </linearGradient>
      <radialGradient id="lamp" cx="0.5" cy="0.95" r="0.85">
        <stop offset="0" stop-color="#ffd9a0" stop-opacity="0.55" />
        <stop offset="0.6" stop-color="#ffc172" stop-opacity="0.12" />
        <stop offset="1" stop-color="#ffb452" stop-opacity="0" />
      </radialGradient>
      <linearGradient id="glass" x1="0" x2="1" y1="0" y2="1">
        <stop offset="0" stop-color="#ffffff" stop-opacity="0.20" />
        <stop offset="0.42" stop-color="#ffffff" stop-opacity="0.05" />
        <stop offset="0.56" stop-color="#000000" stop-opacity="0.05" />
        <stop offset="1" stop-color="#000000" stop-opacity="0.12" />
      </linearGradient>
    </defs>

    <rect x="0" y="0" width="200" height="120" rx="5" fill="url(#face)" />
    <!-- Warm lamp behind the face, the way the hardware's meter is lit. -->
    <rect x="0" y="0" width="200" height="120" rx="5" fill="url(#lamp)" />

    <!-- Scale arc -->
    <path
      class="arc"
      d="M {arcStart.x.toFixed(2)} {arcStart.y.toFixed(2)} A {SCALE_R} {SCALE_R} 0 0 1 {arcEnd.x.toFixed(2)} {arcEnd.y.toFixed(2)}"
    />

    <!-- Red end of the scale, over the arc it shares -->
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
      {@const inner = polar(tick.norm, SCALE_R - (tick.major ? TICK_MAJOR : TICK_MINOR))}
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

    <!-- Needle -->
    <g class="needle" class:parked={!live}>
      <line
        x1={PIVOT_X}
        y1={PIVOT_Y}
        x2={needleTip.x}
        y2={needleTip.y}
      />
      <circle cx={PIVOT_X} cy={PIVOT_Y} r="7" />
    </g>

    <rect x="0" y="0" width="200" height="120" rx="5" fill="url(#glass)" />
    <rect class="bezel" x="0.75" y="0.75" width="198.5" height="118.5" rx="4.5" />
  </svg>

  {#if clip}
    <button type="button" class="clip" onclick={onclearclip} title="Output clipped — click to reset">
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
    border-radius: 5px;
    box-shadow:
      inset 0 2px 6px rgba(60, 36, 12, 0.35),
      0 2px 5px rgba(0, 0, 0, 0.5);
  }

  .arc {
    fill: none;
    stroke: #4a3a22;
    stroke-width: 1.4;
  }

  .arc.hot {
    stroke: #b23a20;
    stroke-width: 2.6;
  }

  .tick {
    stroke: #4a3a22;
    stroke-width: 1.1;
    stroke-linecap: round;
  }

  .tick.major {
    stroke-width: 1.9;
  }

  .tick.hot {
    stroke: #b23a20;
  }

  .label {
    fill: #4a3a22;
    font-family: 'Helvetica Neue', Helvetica, Arial, sans-serif;
    font-size: 8px;
    font-weight: 600;
    text-anchor: middle;
    dominant-baseline: middle;
  }

  .label.hot {
    fill: #b23a20;
  }

  .unit {
    fill: #6b5638;
    font-family: 'Helvetica Neue', Helvetica, Arial, sans-serif;
    font-size: 7px;
    font-weight: 700;
    letter-spacing: 0.16em;
    text-anchor: middle;
  }

  .needle line {
    stroke: #17100a;
    stroke-width: 1.9;
    stroke-linecap: round;
  }

  .needle circle {
    fill: #241a0e;
  }

  .needle.parked line {
    stroke: #8f7a58;
  }

  .bezel {
    fill: none;
    stroke: rgba(40, 24, 8, 0.5);
    stroke-width: 1.5;
  }

  .clip {
    position: absolute;
    top: 8%;
    right: 6%;
    padding: 0.15rem 0.4rem;
    border-radius: 2px;
    background: #c4381c;
    color: #fff;
    font-size: 0.55rem;
    font-weight: 800;
    letter-spacing: 0.12em;
    cursor: pointer;
    box-shadow: 0 0 8px rgba(220, 70, 30, 0.7);
  }

  .offline {
    position: absolute;
    left: 50%;
    bottom: 4%;
    transform: translateX(-50%);
    color: #8f7a58;
    font-size: 0.55rem;
    font-weight: 700;
    letter-spacing: 0.14em;
    text-transform: uppercase;
  }
</style>
