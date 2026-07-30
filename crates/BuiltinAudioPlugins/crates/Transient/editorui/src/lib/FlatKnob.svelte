<script lang="ts">
  import { clamp, format, fromNorm, toNorm, type ParamSpec } from '../params'

  type Props = {
    spec: ParamSpec
    value: number
    onchange: (value: number) => void
    /** CSS size; defaults to `--knob-lg`. */
    size?: string
    disabled?: boolean
  }

  const { spec, value, onchange, size, disabled = false }: Props = $props()

  const norm = $derived(toNorm(spec, value))

  let dragging = $state(false)
  let dragStartY = 0
  let dragStartNorm = 0

  /** Full 0→1 sweep; Shift multiplies for fine control. */
  const TRAVEL_PX = 380

  const START_DEG = -135
  const SWEEP_DEG = 270
  const pointerAngle = $derived(START_DEG + SWEEP_DEG * norm)

  function commit(nextNorm: number) {
    if (disabled) return
    onchange(fromNorm(spec, clamp(nextNorm, 0, 1)))
  }

  function onPointerDown(event: PointerEvent) {
    if (disabled || event.button !== 0) return
    dragging = true
    dragStartY = event.clientY
    dragStartNorm = norm
    ;(event.currentTarget as HTMLElement).setPointerCapture(event.pointerId)
    event.preventDefault()
  }

  function onPointerMove(event: PointerEvent) {
    if (!dragging) return
    const travel = event.shiftKey ? TRAVEL_PX * 4 : TRAVEL_PX
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
    const step = event.shiftKey ? spec.step * 0.25 : spec.step
    onchange(
      clamp(value + (event.deltaY < 0 ? step : -step), spec.min, spec.max),
    )
  }

  function onKeyDown(event: KeyboardEvent) {
    if (disabled) return
    const step = event.shiftKey ? spec.step * 0.25 : spec.step
    if (event.key === 'ArrowUp' || event.key === 'ArrowRight') {
      event.preventDefault()
      onchange(clamp(value + step, spec.min, spec.max))
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowLeft') {
      event.preventDefault()
      onchange(clamp(value - step, spec.min, spec.max))
    } else if (event.key === 'Home') {
      event.preventDefault()
      onchange(spec.max)
    } else if (event.key === 'End') {
      event.preventDefault()
      onchange(spec.min)
    }
  }
</script>

<div
  class="knob"
  class:disabled
  class:dragging
  style={size ? `--knob: ${size}` : undefined}
>
  <div
    class="cap"
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
    <svg viewBox="0 0 80 80" aria-hidden="true">
      <circle
        class="track"
        cx="40"
        cy="40"
        r="30"
        fill="none"
        pathLength="100"
        stroke-dasharray="75 25"
        transform="rotate(135 40 40)"
      />
      <circle
        class="fill"
        cx="40"
        cy="40"
        r="30"
        fill="none"
        pathLength="100"
        stroke-dasharray="{(norm * 75).toFixed(2)} 100"
        transform="rotate(135 40 40)"
      />
      <circle cx="40" cy="40" r="22" class="body" />
      <circle cx="40" cy="40" r="17.5" class="face" />
      <g style="transform: rotate({pointerAngle}deg); transform-origin: 40px 40px">
        <rect class="pointer" x="39" y="14" width="2" height="10" rx="1" />
      </g>
    </svg>
  </div>
  <div class="plate">
    <div class="value">{format(spec, value)}<small>{spec.unit}</small></div>
    <div class="name">{spec.label}</div>
  </div>
</div>

<style>
  .knob {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.35rem;
    min-width: 0;
  }

  .knob.disabled {
    opacity: 0.35;
    pointer-events: none;
  }

  .cap {
    width: var(--knob, var(--knob-lg));
    height: var(--knob, var(--knob-lg));
    cursor: ns-resize;
    touch-action: none;
    outline: none;
    filter: drop-shadow(0 0.28rem 0.32rem rgba(0, 0, 0, 0.5));
  }

  svg {
    display: block;
    width: 100%;
    height: 100%;
  }

  .track {
    stroke: rgba(231, 239, 243, 0.11);
    stroke-width: 4;
    stroke-linecap: round;
  }

  .fill {
    stroke: var(--accent);
    stroke-width: 4;
    stroke-linecap: round;
  }

  .body {
    fill: #11161a;
    stroke: rgba(255, 255, 255, 0.13);
    stroke-width: 1;
  }

  .face {
    fill: #20272d;
    stroke: rgba(0, 0, 0, 0.5);
    stroke-width: 1;
  }

  .pointer {
    fill: #f4f7f8;
  }

  .knob.dragging .fill {
    stroke: var(--accent-soft);
  }

  .knob.dragging .pointer {
    fill: var(--accent-soft);
  }

  .plate {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.1rem;
  }

  .value {
    color: var(--text);
    font-size: 0.74rem;
    font-weight: 680;
    letter-spacing: -0.02em;
  }

  .value small {
    margin-left: 0.12rem;
    color: var(--text-muted);
    font-size: 0.52rem;
  }

  .name {
    color: var(--text-faint);
    font-size: 0.55rem;
    font-weight: 650;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }
</style>
