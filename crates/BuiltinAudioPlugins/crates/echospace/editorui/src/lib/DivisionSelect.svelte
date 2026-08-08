<script lang="ts">
  import { DIVISION_LABELS, divisionMs } from '../params'

  type Props = {
    label: string
    /** Index into `DIVISION_LABELS`; the wire value Rust indexes with. */
    value: number
    /** Transport tempo, for the millisecond readout under the note. */
    tempoBpm: number
    onchange: (value: number) => void
    disabled?: boolean
  }

  const { label, value, tempoBpm, onchange, disabled = false }: Props = $props()

  const last = DIVISION_LABELS.length - 1
  const index = $derived(Math.min(Math.max(Math.round(value), 0), last))
  const ms = $derived(divisionMs(index, tempoBpm))

  /**
   * The readout is what the delay line is really running at, tempo included —
   * a note name alone hides the moment a long division clamps against the
   * line's 4 s ceiling.
   */
  const readout = $derived(
    ms >= 1000 ? `${(ms / 1000).toFixed(2)} s` : `${Math.round(ms)} ms`,
  )

  function step(delta: number) {
    const next = index + delta
    if (next < 0 || next > last) return
    onchange(next)
  }

  function onKeyDown(event: KeyboardEvent) {
    if (event.key === 'ArrowUp' || event.key === 'ArrowRight') {
      event.preventDefault()
      step(1)
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowLeft') {
      event.preventDefault()
      step(-1)
    }
  }
</script>

<div class="division" class:disabled>
  <div class="row">
    <button
      type="button"
      class="step"
      aria-label="Shorter {label} division"
      disabled={disabled || index === 0}
      onclick={() => step(-1)}
    >
      ‹
    </button>
    <label class="select">
      <span class="note">{DIVISION_LABELS[index]}</span>
      <select
        aria-label="{label} division"
        {disabled}
        value={index}
        onkeydown={onKeyDown}
        onchange={(event) =>
          onchange(Number((event.currentTarget as HTMLSelectElement).value))}
      >
        {#each DIVISION_LABELS as name, option (name)}
          <option value={option}>{name}</option>
        {/each}
      </select>
    </label>
    <button
      type="button"
      class="step"
      aria-label="Longer {label} division"
      disabled={disabled || index === last}
      onclick={() => step(1)}
    >
      ›
    </button>
  </div>
  <div class="readout">{readout}</div>
  <div class="label">{label}</div>
</div>

<style>
  .division {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.25rem;
    min-width: 0;
    width: 100%;
  }

  .division.disabled {
    opacity: 0.45;
  }

  .row {
    display: grid;
    grid-template-columns: 1.35rem minmax(0, 1fr) 1.35rem;
    align-items: stretch;
    width: min(100%, 7.5rem);
    height: 1.9rem;
    overflow: hidden;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: var(--base);
  }

  .step {
    color: var(--text-muted);
    font-size: 0.95rem;
    line-height: 1;
    cursor: pointer;
  }

  .step:hover:not(:disabled) {
    color: var(--text);
    background: var(--raised);
  }

  .step:disabled {
    cursor: default;
    opacity: 0.35;
  }

  .select {
    position: relative;
    display: grid;
    place-items: center;
    min-width: 0;
    border-inline: 1px solid var(--border);
  }

  .note {
    overflow: hidden;
    width: 100%;
    color: var(--accent);
    font-size: 0.76rem;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
    text-align: center;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* The real control sits invisibly on top so the native picker (and its
     keyboard handling) does the work, with our own face drawn underneath —
     the same trick `PresetControl` uses. */
  .select select {
    position: absolute;
    inset: 0;
    width: 100%;
    border: 0;
    opacity: 0;
    cursor: pointer;
    background: var(--raised);
    color: var(--text);
  }

  .select select:disabled {
    cursor: default;
  }

  .readout {
    color: var(--text-muted);
    font-size: 0.7rem;
    font-variant-numeric: tabular-nums;
  }

  .label {
    color: var(--text-faint);
    font-size: 0.66rem;
    font-weight: 650;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
</style>
