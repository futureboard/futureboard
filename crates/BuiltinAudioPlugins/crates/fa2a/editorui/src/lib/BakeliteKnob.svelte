<script lang="ts">
  import { clamp, format, fromNorm, toNorm, type ParamSpec } from '../params'

  type Props = {
    spec: ParamSpec
    value: number
    onchange: (value: number) => void
    /** CSS size; defaults to the panel's `--knob-lg`. */
    size?: string
    /** Numbered aluminum skirt — Peak Reduction / Gain on the hardware. */
    dial?: boolean
    disabled?: boolean
  }

  const {
    spec,
    value,
    onchange,
    size,
    dial = false,
    disabled = false,
  }: Props = $props()

  const norm = $derived(toNorm(spec, value))
  const uid = $derived(`k${spec.id}`)

  let dragging = $state(false)
  let editing = $state(false)
  let draft = $state('')
  let inputEl: HTMLInputElement | undefined = $state()
  let dragStartY = 0
  let dragStartNorm = 0

  $effect(() => {
    if (editing) {
      inputEl?.focus()
      inputEl?.select()
    }
  })

  const TRAVEL_PX = 240

  function commit(nextNorm: number) {
    if (disabled) return
    onchange(fromNorm(spec, clamp(nextNorm, 0, 1)))
  }

  function onPointerDown(event: PointerEvent) {
    if (disabled || editing || event.button !== 0) return
    dragging = true
    dragStartY = event.clientY
    dragStartNorm = norm
    ;(event.currentTarget as HTMLElement).setPointerCapture(event.pointerId)
    event.preventDefault()
  }

  function onPointerMove(event: PointerEvent) {
    if (!dragging) return
    const travel = event.shiftKey ? TRAVEL_PX * 5 : TRAVEL_PX
    commit(dragStartNorm + (dragStartY - event.clientY) / travel)
  }

  function onPointerUp(event: PointerEvent) {
    if (!dragging) return
    dragging = false
    ;(event.currentTarget as HTMLElement).releasePointerCapture(event.pointerId)
  }

  function onWheel(event: WheelEvent) {
    if (disabled) return
    event.preventDefault()
    const step = event.shiftKey ? spec.step / 5 : spec.step
    onchange(
      clamp(value + (event.deltaY < 0 ? step : -step), spec.min, spec.max),
    )
  }

  function onKeyDown(event: KeyboardEvent) {
    if (disabled) return
    const step = event.shiftKey ? spec.step / 5 : spec.step
    switch (event.key) {
      case 'ArrowUp':
      case 'ArrowRight':
        onchange(clamp(value + step, spec.min, spec.max))
        break
      case 'ArrowDown':
      case 'ArrowLeft':
        onchange(clamp(value - step, spec.min, spec.max))
        break
      case 'PageUp':
        commit(norm + 0.1)
        break
      case 'PageDown':
        commit(norm - 0.1)
        break
      case 'Home':
        onchange(spec.default)
        break
      default:
        return
    }
    event.preventDefault()
  }

  function startEdit() {
    if (disabled) return
    draft = String(Number(value.toFixed(Math.max(spec.digits, 2))))
    editing = true
  }

  function commitEdit() {
    const parsed = Number(draft.replace(/[^\d.+-]/g, ''))
    if (Number.isFinite(parsed)) onchange(clamp(parsed, spec.min, spec.max))
    editing = false
  }

  // 300° travel opening at the bottom — classic panel pot.
  const START_DEG = -240
  const SWEEP_DEG = 300

  function polar(deg: number, radius: number) {
    const rad = (deg * Math.PI) / 180
    return { x: 50 + Math.cos(rad) * radius, y: 50 + Math.sin(rad) * radius }
  }

  const pointerAngle = $derived(START_DEG + SWEEP_DEG * norm)
  const skirtTicks = Array.from({ length: 11 }, (_, i) => {
    const deg = START_DEG + SWEEP_DEG * (i / 10)
    return {
      a: polar(deg, 46.5),
      b: polar(deg, i % 5 === 0 ? 41 : 43.5),
      label: i % 5 === 0 ? String(i) : '',
      text: polar(deg, 36),
      major: i % 5 === 0,
    }
  })
</script>

<div
  class="knob"
  class:dial
  class:disabled
  style={size ? `--knob: ${size}` : undefined}
>
  <div
    class="cap"
    class:dragging
    role="slider"
    tabindex={disabled ? -1 : 0}
    aria-label={spec.label}
    aria-valuemin={spec.min}
    aria-valuemax={spec.max}
    aria-valuenow={value}
    aria-valuetext="{format(spec, value)} {spec.unit}"
    aria-disabled={disabled}
    title="{spec.label} — drag vertically, Shift for fine, double-click to reset"
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onpointercancel={onPointerUp}
    onwheel={onWheel}
    onkeydown={onKeyDown}
    ondblclick={() => !disabled && onchange(spec.default)}
  >
    <svg viewBox="0 0 100 100" aria-hidden="true">
      <defs>
        <radialGradient id="{uid}-body" cx="0.34" cy="0.26" r="0.8">
          <stop offset="0" stop-color="#454440" />
          <stop offset="0.45" stop-color="#242321" />
          <stop offset="1" stop-color="#0b0b0a" />
        </radialGradient>
        <linearGradient id="{uid}-ring" x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stop-color="#5c5a55" />
          <stop offset="1" stop-color="#272624" />
        </linearGradient>
      </defs>

      {#if dial}
        <!-- Scale is printed on the faceplate around the bakelite knob. -->
        {#each skirtTicks as tick}
          <line
            class="skirt-tick"
            class:major={tick.major}
            x1={tick.a.x}
            y1={tick.a.y}
            x2={tick.b.x}
            y2={tick.b.y}
          />
          {#if tick.label}
            <text class="skirt-label" x={tick.text.x} y={tick.text.y}>
              {tick.label}
            </text>
          {/if}
        {/each}
        <circle class="ring" cx="50" cy="50" r="31" fill="url(#{uid}-ring)" />
      {:else}
        <circle class="ring" cx="50" cy="50" r="30" fill="url(#{uid}-ring)" />
      {/if}

      <g style="transform: rotate({pointerAngle + 90}deg); transform-origin: 50px 50px">
        <circle
          class="body"
          cx="50"
          cy="50"
          r={dial ? 27 : 26}
          fill="url(#{uid}-body)"
        />
        <path
          class="gloss"
          d={dial
            ? 'M 36 41 A 17 17 0 0 1 63 37'
            : 'M 33 38 A 21 21 0 0 1 66 33'}
        />
        <rect
          class="flute"
          x="48.6"
          y={dial ? 25.5 : 26}
          width="2.8"
          height={dial ? 18.5 : 15}
          rx="1.4"
        />
      </g>
    </svg>
  </div>

  <div class="plate">
    <div class="name">{spec.label}</div>
    {#if editing}
      <input
        class="input"
        bind:this={inputEl}
        bind:value={draft}
        aria-label="{spec.label} value"
        onblur={commitEdit}
        onkeydown={(event) => {
          if (event.key === 'Enter') commitEdit()
          if (event.key === 'Escape') editing = false
        }}
      />
    {:else}
      <button
        type="button"
        class="readout"
        {disabled}
        title="Click to type a value"
        onclick={startEdit}
      >
        {format(spec, value)}<span class="unit">{spec.unit}</span>
      </button>
    {/if}
  </div>
</div>

<style>
  .knob {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.4rem;
    min-width: 0;
  }

  .cap {
    width: var(--knob, var(--knob-lg));
    height: var(--knob, var(--knob-lg));
    cursor: ns-resize;
    touch-action: none;
    outline: none;
    filter: drop-shadow(0 3px 4px rgba(0, 0, 0, 0.32));
  }

  .cap.dragging {
    filter: drop-shadow(0 4px 6px rgba(0, 0, 0, 0.38));
  }

  .knob.disabled .cap {
    cursor: default;
    opacity: 0.4;
    filter: none;
  }

  svg {
    display: block;
    width: 100%;
    height: 100%;
    overflow: visible;
  }

  .ring {
    stroke: rgba(0, 0, 0, 0.4);
    stroke-width: 0.8;
  }

  .body {
    stroke: rgba(0, 0, 0, 0.65);
    stroke-width: 1;
  }

  .gloss {
    fill: none;
    stroke: rgba(255, 245, 225, 0.16);
    stroke-width: 1.8;
    stroke-linecap: round;
  }

  .flute {
    fill: #f4ead4;
  }

  .skirt-tick {
    stroke: var(--engrave-muted);
    stroke-width: 1.1;
    stroke-linecap: round;
  }

  .skirt-tick.major {
    stroke-width: 1.9;
  }

  .skirt-label {
    fill: var(--engrave);
    font-size: 7.5px;
    font-weight: 700;
    text-anchor: middle;
    dominant-baseline: middle;
  }

  .plate {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.12rem;
  }

  .name {
    max-width: 8rem;
    color: var(--engrave);
    font-size: 0.76rem;
    font-weight: 750;
    letter-spacing: 0.1em;
    line-height: 1.15;
    text-transform: uppercase;
    text-align: center;
    text-shadow: 0 1px 0 rgba(255, 248, 235, 0.12);
  }

  .knob:not(.dial) .name {
    color: var(--engrave-muted);
    font-size: 0.65rem;
    font-weight: 650;
    letter-spacing: 0.06em;
  }

  .readout {
    padding: 0.1rem 0.4rem;
    border-radius: var(--radius-sm);
    color: var(--readout);
    font-size: 0.84rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    cursor: text;
  }

  .knob:not(.dial) .readout {
    font-size: 0.72rem;
    opacity: 0.9;
  }

  .readout:hover {
    background: rgba(0, 0, 0, 0.1);
  }

  .unit {
    margin-left: 0.1rem;
    font-size: 0.6rem;
    opacity: 0.6;
  }

  .input {
    width: 3.6rem;
    padding: 0.06rem 0.2rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    outline: none;
    background: #efe6d3;
    color: #1d1a15;
    font-size: 0.7rem;
    font-weight: 600;
    text-align: center;
    user-select: text;
  }
</style>
