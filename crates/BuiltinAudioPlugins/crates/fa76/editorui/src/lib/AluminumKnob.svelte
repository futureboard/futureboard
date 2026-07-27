<script lang="ts">
  import { clamp, format, fromNorm, toNorm, type ParamSpec } from '../params'

  type Props = {
    spec: ParamSpec
    value: number
    onchange: (value: number) => void
    /** CSS size; defaults to the panel's `--knob-lg`. */
    size?: string
    disabled?: boolean
  }

  const { spec, value, onchange, size, disabled = false }: Props = $props()

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

  const START_DEG = -240
  const SWEEP_DEG = 300
  const pointerAngle = $derived(START_DEG + SWEEP_DEG * norm)
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
        <radialGradient id="{uid}-body" cx="0.32" cy="0.28" r="0.78">
          <stop offset="0" stop-color="#d8dee8" />
          <stop offset="0.45" stop-color="#9aa4b4" />
          <stop offset="1" stop-color="#4a5360" />
        </radialGradient>
        <linearGradient id="{uid}-rim" x1="0" x2="0" y1="0" y2="1">
          <stop offset="0" stop-color="#eef2f8" />
          <stop offset="0.5" stop-color="#8a95a6" />
          <stop offset="1" stop-color="#3a4250" />
        </linearGradient>
      </defs>

      <circle class="rim" cx="50" cy="50" r="31" fill="url(#{uid}-rim)" />
      <g style="transform: rotate({pointerAngle + 90}deg); transform-origin: 50px 50px">
        <circle class="body" cx="50" cy="50" r="26" fill="url(#{uid}-body)" />
        <path class="gloss" d="M 34 38 A 20 20 0 0 1 66 34" />
        <rect class="pointer" x="48.7" y="24" width="2.6" height="16" rx="1.2" />
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
    gap: 0.35rem;
    min-width: 0;
  }

  .cap {
    width: var(--knob, var(--knob-lg));
    height: var(--knob, var(--knob-lg));
    cursor: ns-resize;
    touch-action: none;
    outline: none;
    filter: drop-shadow(0 4px 6px rgba(0, 0, 0, 0.55));
  }

  .cap.dragging {
    filter: drop-shadow(0 5px 8px rgba(0, 0, 0, 0.65));
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

  .rim {
    stroke: rgba(0, 0, 0, 0.55);
    stroke-width: 1;
  }

  .body {
    stroke: rgba(20, 24, 32, 0.7);
    stroke-width: 1;
  }

  .gloss {
    fill: none;
    stroke: rgba(255, 255, 255, 0.35);
    stroke-width: 2;
    stroke-linecap: round;
  }

  .pointer {
    fill: #11151c;
  }

  .plate {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.1rem;
  }

  .name {
    color: var(--engrave);
    font-size: 0.7rem;
    font-weight: 700;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    text-align: center;
  }

  .readout {
    padding: 0.08rem 0.35rem;
    border-radius: var(--radius-sm);
    color: var(--readout);
    font-size: 0.78rem;
    font-weight: 600;
    cursor: text;
  }

  .readout:hover {
    background: rgba(255, 255, 255, 0.06);
  }

  .unit {
    margin-left: 0.1rem;
    font-size: 0.58rem;
    opacity: 0.65;
  }

  .input {
    width: 3.6rem;
    padding: 0.06rem 0.2rem;
    border: 1px solid var(--border-hi);
    border-radius: var(--radius-sm);
    outline: none;
    background: #0c1016;
    color: var(--engrave);
    font-size: 0.7rem;
    font-weight: 600;
    text-align: center;
    user-select: text;
  }
</style>
