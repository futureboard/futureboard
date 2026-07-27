<script lang="ts">
  import {
    clamp,
    format,
    fromNorm,
    toNorm,
    type ParamSpec,
  } from '../params'

  type Props = {
    spec: ParamSpec
    value: number
    onchange: (value: number) => void
    /** Optional CSS size override, e.g. `5.5rem`. Defaults to `--knob-size`. */
    size?: string
    alert?: boolean
    disabled?: boolean
  }

  const {
    spec,
    value,
    onchange,
    size,
    alert = false,
    disabled = false,
  }: Props = $props()

  const norm = $derived(toNorm(spec, value))
  const uid = `k${Math.random().toString(36).slice(2, 9)}`

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
    if (Number.isFinite(parsed)) {
      onchange(clamp(parsed, spec.min, spec.max))
    }
    editing = false
  }

  const START_DEG = -225
  const SWEEP_DEG = 270
  const R = 41

  function polar(deg: number, radius: number) {
    const rad = (deg * Math.PI) / 180
    return { x: 50 + Math.cos(rad) * radius, y: 50 + Math.sin(rad) * radius }
  }

  function arc(fromNormValue: number, toNormValue: number, radius: number) {
    const a0 = START_DEG + SWEEP_DEG * fromNormValue
    const a1 = START_DEG + SWEEP_DEG * toNormValue
    const p0 = polar(a0, radius)
    const p1 = polar(a1, radius)
    const large = Math.abs(a1 - a0) > 180 ? 1 : 0
    const sweep = a1 >= a0 ? 1 : 0
    return `M ${p0.x.toFixed(2)} ${p0.y.toFixed(2)} A ${radius} ${radius} 0 ${large} ${sweep} ${p1.x.toFixed(2)} ${p1.y.toFixed(2)}`
  }

  const originNorm = $derived(
    spec.origin === undefined ? 0 : toNorm(spec, spec.origin),
  )
  const defaultNorm = $derived(toNorm(spec, spec.default))
  const pointer = $derived(polar(START_DEG + SWEEP_DEG * norm, R - 7))
  const inner = $derived(polar(START_DEG + SWEEP_DEG * norm, R - 24))
  const tip = $derived(polar(START_DEG + SWEEP_DEG * norm, R - 10))
  const defaultTick = $derived(polar(START_DEG + SWEEP_DEG * defaultNorm, 47))
  const defaultTickOuter = $derived(
    polar(START_DEG + SWEEP_DEG * defaultNorm, 51),
  )

  const ticks = [0, 0.25, 0.5, 0.75, 1].map((t) => {
    const deg = START_DEG + SWEEP_DEG * t
    return { a: polar(deg, 46.5), b: polar(deg, 49.5) }
  })
</script>

<div
  class="knob"
  class:disabled
  style={size ? `--knob-size: ${size}` : undefined}
>
  <div class="label">{spec.label}</div>
  <div
    class="dial"
    class:dragging
    class:alert
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
        <linearGradient id="{uid}-ring" x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stop-color="#3a3348" />
          <stop offset="1" stop-color="#15121c" />
        </linearGradient>
        <linearGradient id="{uid}-body" x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stop-color="#2c2538" />
          <stop offset=".45" stop-color="#1a1622" />
          <stop offset="1" stop-color="#0c0a10" />
        </linearGradient>
        <radialGradient id="{uid}-cap" cx=".32" cy=".24" r=".78">
          <stop offset="0" stop-color="#4a4060" />
          <stop offset=".28" stop-color="#2a2336" />
          <stop offset=".7" stop-color="#16121c" />
          <stop offset="1" stop-color="#0a0810" />
        </radialGradient>
        <radialGradient id="{uid}-glow" cx=".5" cy=".5" r=".5">
          <stop offset="0" stop-color="#9b7dff" stop-opacity=".35" />
          <stop offset="1" stop-color="#9b7dff" stop-opacity="0" />
        </radialGradient>
        <filter id="{uid}-shadow" x="-35%" y="-35%" width="170%" height="180%">
          <feDropShadow
            dx="0"
            dy="3.5"
            stdDeviation="3.2"
            flood-color="#000"
            flood-opacity=".55"
          />
        </filter>
      </defs>

      {#each ticks as tick}
        <line class="tick" x1={tick.a.x} y1={tick.a.y} x2={tick.b.x} y2={tick.b.y} />
      {/each}

      <path class="track" d={arc(0, 1, R)} />
      {#if Math.abs(norm - originNorm) > 0.001}
        <path class="fill" d={arc(originNorm, norm, R)} />
      {/if}
      <line
        class="default"
        x1={defaultTick.x}
        y1={defaultTick.y}
        x2={defaultTickOuter.x}
        y2={defaultTickOuter.y}
      />

      <circle class="ring" cx="50" cy="50" r="35.5" fill="url(#{uid}-ring)" />
      <circle
        class="body"
        cx="50"
        cy="50"
        r="32.5"
        fill="url(#{uid}-body)"
        filter="url(#{uid}-shadow)"
      />
      <circle class="cap" cx="50" cy="50" r="27.5" fill="url(#{uid}-cap)" />
      <circle class="rim" cx="50" cy="50" r="27.5" />
      <path class="highlight" d="M30 39 A 22 22 0 0 1 67 31" />
      {#if dragging}
        <circle cx="50" cy="50" r="36" fill="url(#{uid}-glow)" />
      {/if}
      <line
        class="pointer"
        x1={inner.x}
        y1={inner.y}
        x2={pointer.x}
        y2={pointer.y}
      />
      <circle class="tip" cx={tip.x} cy={tip.y} r="2.1" />
    </svg>
  </div>

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
      <span class="value">{format(spec, value)}</span>
      <span class="unit">{spec.unit}</span>
    </button>
  {/if}
</div>

<style>
  .knob {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.3rem;
    width: 100%;
    max-width: 5.4rem;
    min-width: 0;
  }

  .label {
    max-width: 100%;
    overflow: hidden;
    color: var(--text-muted);
    font-size: 0.68rem;
    font-weight: 600;
    letter-spacing: 0.01em;
    text-align: center;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dial {
    width: var(--knob-size);
    height: var(--knob-size);
    cursor: ns-resize;
    touch-action: none;
    outline: none;
    transition: transform 120ms ease;
  }

  .dial:hover {
    transform: scale(1.025);
  }

  .dial.dragging {
    transform: scale(1.035);
  }

  .knob.disabled .dial {
    cursor: default;
    opacity: 0.36;
    transform: none;
  }

  svg {
    display: block;
    width: 100%;
    height: 100%;
    overflow: visible;
  }

  .tick {
    stroke: rgba(255, 255, 255, 0.14);
    stroke-width: 1.25;
    stroke-linecap: round;
  }

  .track {
    fill: none;
    stroke: rgba(255, 255, 255, 0.08);
    stroke-width: 3.75;
    stroke-linecap: round;
  }

  .fill {
    fill: none;
    stroke: var(--accent);
    stroke-width: 3.75;
    stroke-linecap: round;
    filter: drop-shadow(
      0 0 2.5px color-mix(in srgb, var(--accent) 60%, transparent)
    );
  }

  .default {
    stroke: rgba(255, 255, 255, 0.28);
    stroke-width: 1.5;
    stroke-linecap: round;
  }

  .ring {
    stroke: rgba(255, 255, 255, 0.06);
    stroke-width: 0.75;
  }

  .body {
    stroke: rgba(255, 255, 255, 0.1);
    stroke-width: 1;
  }

  .cap {
    stroke: rgba(255, 255, 255, 0.035);
    stroke-width: 1;
  }

  .rim {
    fill: none;
    stroke: rgba(155, 125, 255, 0.12);
    stroke-width: 1.1;
  }

  .highlight {
    fill: none;
    stroke: rgba(255, 255, 255, 0.2);
    stroke-width: 1.35;
    stroke-linecap: round;
  }

  .pointer {
    stroke: #f3effc;
    stroke-width: 2.4;
    stroke-linecap: round;
  }

  .tip {
    fill: var(--accent-bright);
    stroke: #fff;
    stroke-width: 0.6;
  }

  .dial.dragging .fill,
  .dial:hover .fill {
    stroke: var(--accent-bright);
  }

  .dial.dragging .body {
    stroke: color-mix(in srgb, var(--accent) 50%, transparent);
  }

  .dial.dragging .rim {
    stroke: color-mix(in srgb, var(--accent) 45%, transparent);
  }

  .dial.alert .fill {
    stroke: var(--warn);
    filter: none;
  }

  .dial.alert .tip {
    fill: var(--warn);
  }

  .readout {
    display: flex;
    align-items: baseline;
    justify-content: center;
    gap: 0.18rem;
    min-width: 4rem;
    min-height: 1.3rem;
    padding: 0.12rem 0.3rem;
    border-radius: var(--radius-sm);
    cursor: text;
  }

  .readout:hover {
    background: rgba(255, 255, 255, 0.05);
  }

  .value {
    color: var(--text);
    font-size: 0.8rem;
    font-weight: 600;
  }

  .unit {
    color: var(--text-faint);
    font-size: 0.62rem;
  }

  .input {
    width: 4.2rem;
    min-height: 1.3rem;
    padding: 0.12rem 0.3rem;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    outline: none;
    background: var(--base);
    color: var(--text);
    font-size: 0.8rem;
    font-weight: 600;
    text-align: center;
    user-select: text;
  }
</style>
