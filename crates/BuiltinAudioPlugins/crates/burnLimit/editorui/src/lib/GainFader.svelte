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

  let dragging = $state(false)
  /** Relative fine-drag origin (shift). Absolute track mapping otherwise. */
  let fine = false
  let fineStartY = 0
  let fineStartNorm = 0

  /** Pixels of mouse travel for a full 0→1 sweep while holding Shift. */
  const FINE_TRAVEL_PX = 720

  function commit(nextNorm: number) {
    if (disabled) return
    onchange(fromNorm(spec, clamp(nextNorm, 0, 1)))
  }

  function normFromPointer(event: PointerEvent, track: HTMLElement): number {
    const rect = track.getBoundingClientRect()
    if (rect.height <= 0) return norm
    return clamp(1 - (event.clientY - rect.top) / rect.height, 0, 1)
  }

  function onPointerDown(event: PointerEvent) {
    if (disabled || event.button !== 0) return
    const track = event.currentTarget as HTMLElement
    dragging = true
    fine = event.shiftKey
    if (fine) {
      fineStartY = event.clientY
      fineStartNorm = norm
    } else {
      commit(normFromPointer(event, track))
    }
    track.setPointerCapture(event.pointerId)
    event.preventDefault()
  }

  function onPointerMove(event: PointerEvent) {
    if (!dragging) return
    const track = event.currentTarget as HTMLElement
    if (fine || event.shiftKey) {
      if (!fine) {
        fine = true
        fineStartY = event.clientY
        fineStartNorm = norm
      }
      const delta = (fineStartY - event.clientY) / FINE_TRAVEL_PX
      commit(fineStartNorm + delta)
      return
    }
    fine = false
    commit(normFromPointer(event, track))
  }

  function onPointerUp(event: PointerEvent) {
    if (!dragging) return
    dragging = false
    fine = false
    ;(event.currentTarget as HTMLElement).releasePointerCapture(event.pointerId)
  }

  function onWheel(event: WheelEvent) {
    if (disabled) return
    event.preventDefault()
    // Gain is coarse in dB; default wheel is half a step, Shift is finer.
    const step = event.shiftKey ? spec.step * 0.25 : spec.step * 0.5
    onchange(
      clamp(value + (event.deltaY < 0 ? step : -step), spec.min, spec.max),
    )
  }

  function onKeyDown(event: KeyboardEvent) {
    if (disabled) return
    const fineStep = event.shiftKey ? spec.step * 0.25 : spec.step
    if (event.key === 'ArrowUp' || event.key === 'ArrowRight') {
      event.preventDefault()
      onchange(clamp(value + fineStep, spec.min, spec.max))
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowLeft') {
      event.preventDefault()
      onchange(clamp(value - fineStep, spec.min, spec.max))
    } else if (event.key === 'Home') {
      event.preventDefault()
      onchange(spec.max)
    } else if (event.key === 'End') {
      event.preventDefault()
      onchange(spec.min)
    }
  }
</script>

<div class="fader" class:disabled class:dragging>
  <div class="readout">{format(spec, value)}<small>{spec.unit}</small></div>
  <div
    class="track"
    role="slider"
    tabindex={disabled ? -1 : 0}
    aria-label={spec.label}
    aria-valuemin={spec.min}
    aria-valuemax={spec.max}
    aria-valuenow={value}
    aria-valuetext="{format(spec, value)} {spec.unit}"
    aria-orientation="vertical"
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onpointercancel={onPointerUp}
    onwheel={onWheel}
    onkeydown={onKeyDown}
    ondblclick={() => !disabled && onchange(spec.default)}
  >
    <div class="fill" style="height: {(norm * 100).toFixed(2)}%"></div>
    <div class="thumb" style="bottom: {(norm * 100).toFixed(2)}%"></div>
  </div>
  <div class="caption">{spec.label}</div>
</div>

<style>
  .fader {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--s2);
    width: 100%;
    height: 100%;
    padding: var(--s3) var(--s2) var(--s2);
  }

  .fader.disabled {
    opacity: 0.35;
    pointer-events: none;
  }

  .readout {
    min-width: 3.2rem;
    padding: 0.2rem 0.35rem;
    border-radius: var(--r-sm);
    background: rgba(255, 255, 255, 0.04);
    color: var(--text);
    font-size: 0.8rem;
    font-weight: 650;
    letter-spacing: -0.02em;
    text-align: center;
  }

  .readout small {
    margin-left: 0.15rem;
    color: var(--text-muted);
    font-size: 0.55rem;
    font-weight: 600;
  }

  .fader.dragging .readout {
    color: var(--steel);
    background: rgba(110, 176, 201, 0.1);
  }

  .track {
    position: relative;
    flex: 1;
    width: 0.7rem;
    min-height: 0;
    border-radius: 999px;
    background: rgba(0, 0, 0, 0.45);
    box-shadow: inset 0 1px 2px rgba(0, 0, 0, 0.45);
    cursor: ns-resize;
    touch-action: none;
    outline: none;
  }

  .fill {
    position: absolute;
    right: 0;
    bottom: 0;
    left: 0;
    border-radius: inherit;
    background: linear-gradient(180deg, #8ec8dc, var(--steel) 55%, var(--steel-deep));
    pointer-events: none;
    transition: height var(--ease);
  }

  .thumb {
    position: absolute;
    left: 50%;
    width: 1.25rem;
    height: 1.25rem;
    transform: translate(-50%, 50%);
    border-radius: 50%;
    background: #f5f7fa;
    border: 2px solid #0e1216;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.35);
    pointer-events: none;
    transition:
      bottom var(--ease),
      border-color var(--ease);
  }

  .fader.dragging .thumb {
    border-color: var(--steel);
  }

  .fader.dragging .fill,
  .fader.dragging .thumb {
    transition-duration: 0ms;
  }

  .caption {
    color: var(--text-muted);
    font-size: 0.55rem;
    font-weight: 650;
    letter-spacing: 0.14em;
    text-transform: uppercase;
  }
</style>
