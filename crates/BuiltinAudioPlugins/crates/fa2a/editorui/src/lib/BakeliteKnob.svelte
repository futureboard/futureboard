<script lang="ts">
  import { clamp, format, fromNorm, toNorm, type ParamSpec } from '../params'

  type Props = {
    spec: ParamSpec
    value: number
    onchange: (value: number) => void
    /** CSS size; defaults to the panel's `--knob-lg`. */
    size?: string
    /** Numbered skirt, the way the hardware's two main controls are marked. */
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

  const TRAVEL_PX = 230

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

  // 300° of travel, opening at the bottom, like a panel-mount potentiometer.
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
      a: polar(deg, 45),
      b: polar(deg, i % 5 === 0 ? 38 : 41),
      label: i % 5 === 0 ? String(i) : '',
      text: polar(deg, 32),
      major: i % 5 === 0,
    }
  })
</script>

<div class="knob" class:disabled style={size ? `--knob: ${size}` : undefined}>
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
        <radialGradient id="{spec.id}-body" cx="0.36" cy="0.28" r="0.82">
          <stop offset="0" stop-color="#4a453f" />
          <stop offset="0.4" stop-color="#282521" />
          <stop offset="0.82" stop-color="#141210" />
          <stop offset="1" stop-color="#0a0908" />
        </radialGradient>
        <linearGradient id="{spec.id}-chrome" x1="0" x2="0.4" y1="0" y2="1">
          <stop offset="0" stop-color="#d8cdb8" />
          <stop offset="0.35" stop-color="#8d8272" />
          <stop offset="0.62" stop-color="#c9bda6" />
          <stop offset="1" stop-color="#6d6456" />
        </linearGradient>
      </defs>

      {#if dial}
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
      {/if}

      <!-- Chrome skirt, then the bakelite cap that turns on it -->
      <circle class="skirt" cx="50" cy="50" r={dial ? 26 : 30} fill="url(#{spec.id}-chrome)" />
      <g style="transform: rotate({pointerAngle + 90}deg); transform-origin: 50px 50px">
        <circle class="body" cx="50" cy="50" r={dial ? 22 : 26} fill="url(#{spec.id}-body)" />
        <path
          class="gloss"
          d={dial
            ? 'M 36 40 A 17 17 0 0 1 63 36'
            : 'M 33 38 A 21 21 0 0 1 66 33'}
        />
        <!-- Pointer flute, cut into the cap the way a chicken-head knob is -->
        <rect
          class="flute"
          x="48.6"
          y={dial ? 30 : 26}
          width="2.8"
          height={dial ? 13 : 15}
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
    filter: drop-shadow(0 3px 5px rgba(0, 0, 0, 0.55));
  }

  .cap:focus-visible {
    border-radius: 50%;
    box-shadow: 0 0 0 2px var(--focus);
  }

  .knob.disabled .cap {
    cursor: default;
    opacity: 0.4;
  }

  svg {
    display: block;
    width: 100%;
    height: 100%;
    overflow: visible;
  }

  .skirt {
    stroke: rgba(0, 0, 0, 0.45);
    stroke-width: 0.8;
  }

  .body {
    stroke: rgba(0, 0, 0, 0.7);
    stroke-width: 1;
  }

  .gloss {
    fill: none;
    stroke: rgba(255, 245, 225, 0.22);
    stroke-width: 2.2;
    stroke-linecap: round;
  }

  .flute {
    fill: #efe4cd;
  }

  .skirt-tick {
    stroke: var(--engrave);
    stroke-width: 1;
    stroke-linecap: round;
  }

  .skirt-tick.major {
    stroke-width: 1.8;
  }

  .skirt-label {
    fill: var(--engrave);
    font-size: 8.5px;
    font-weight: 700;
    text-anchor: middle;
    dominant-baseline: middle;
  }

  .plate {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.1rem;
  }

  /* Wraps rather than nowrap: an engraved legend like PEAK REDUCTION is wider
     than its knob, and forbidding the wrap makes the label — not the control —
     set the panel's minimum width, which overflowed at the host's smallest
     editor size. */
  .name {
    max-width: 7rem;
    color: var(--engrave);
    font-size: 0.62rem;
    font-weight: 700;
    letter-spacing: 0.12em;
    line-height: 1.15;
    text-transform: uppercase;
    text-align: center;
    text-shadow: 0 1px 0 rgba(255, 250, 240, 0.16);
  }

  .readout {
    padding: 0.05rem 0.3rem;
    border-radius: 2px;
    color: var(--readout);
    font-size: 0.72rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    cursor: text;
  }

  .readout:hover {
    background: rgba(0, 0, 0, 0.12);
  }

  .unit {
    margin-left: 0.1rem;
    font-size: 0.55rem;
    opacity: 0.65;
  }

  .input {
    width: 3.6rem;
    padding: 0.05rem 0.2rem;
    border: 1px solid var(--engrave);
    border-radius: 2px;
    outline: none;
    background: rgba(255, 250, 240, 0.85);
    color: #1d1a15;
    font-size: 0.72rem;
    font-weight: 600;
    text-align: center;
    user-select: text;
  }
</style>
