<script lang="ts">
  import { clamp, format, fromNorm, toNorm, type ParamSpec } from '../params'

  type Props = {
    spec: ParamSpec
    value: number
    onchange: (value: number) => void
    disabled?: boolean
  }

  const { spec, value, onchange, disabled = false }: Props = $props()

  const norm = $derived(toNorm(spec, value))
  const uid = $derived(`pk${spec.id}`)

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

<div class="knob" class:disabled class:dragging>
  <div
    class="cap"
    role="slider"
    tabindex={disabled ? -1 : 0}
    aria-label={spec.label}
    aria-valuemin={spec.min}
    aria-valuemax={spec.max}
    aria-valuenow={value}
    aria-valuetext="{format(spec, value)} {spec.unit}"
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onpointercancel={onPointerUp}
    onwheel={onWheel}
    onkeydown={onKeyDown}
    ondblclick={() => !disabled && onchange(spec.default)}
  >
    <svg viewBox="0 0 80 80" aria-hidden="true">
      <defs>
        <linearGradient id="{uid}-body" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stop-color="#2a323c" />
          <stop offset="1" stop-color="#12161c" />
        </linearGradient>
      </defs>
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
      <circle cx="40" cy="40" r="22" fill="url(#{uid}-body)" class="body" />
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
    width: 3.35rem;
    height: 3.35rem;
    cursor: ns-resize;
    touch-action: none;
    outline: none;
  }

  svg {
    display: block;
    width: 100%;
    height: 100%;
  }

  .track {
    stroke: rgba(255, 255, 255, 0.1);
    stroke-width: 4;
    stroke-linecap: round;
  }

  .fill {
    stroke: var(--steel);
    stroke-width: 4;
    stroke-linecap: round;
  }

  .body {
    stroke: rgba(255, 255, 255, 0.08);
    stroke-width: 1;
  }

  .pointer {
    fill: var(--text);
  }

  .knob.dragging .fill {
    stroke: #8ec8dc;
  }

  .knob.dragging .pointer {
    fill: var(--steel);
  }

  .plate {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.1rem;
  }

  .value {
    color: var(--text);
    font-size: 0.72rem;
    font-weight: 650;
    letter-spacing: -0.02em;
  }

  .value small {
    margin-left: 0.12rem;
    color: var(--text-muted);
    font-size: 0.52rem;
  }

  .name {
    color: var(--text-muted);
    font-size: 0.55rem;
    font-weight: 650;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }
</style>
