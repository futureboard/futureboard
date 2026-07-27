<script lang="ts">
  import { MODES, MODE_LABELS, type Mode } from '../params'

  type Props = {
    value: Mode
    onchange: (mode: Mode) => void
    disabled?: boolean
  }

  const { value, onchange, disabled = false }: Props = $props()
</script>

<div class="modes" class:disabled role="radiogroup" aria-label="Delay mode">
  {#each MODES as mode (mode)}
    <button
      type="button"
      role="radio"
      aria-checked={mode === value}
      class:active={mode === value}
      {disabled}
      onclick={() => onchange(mode)}
    >
      {MODE_LABELS[mode]}
    </button>
  {/each}
</div>

<style>
  .modes {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 2px;
    width: min(100%, 24rem);
    padding: 3px;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--base);
  }

  button {
    appearance: none;
    border: none;
    min-width: 0;
    min-height: 1.75rem;
    padding: 0.3rem 0.35rem;
    font: inherit;
    font-size: 0.72rem;
    font-weight: 600;
    color: var(--text-muted);
    background: transparent;
    border-radius: calc(var(--radius-sm) - 1px);
    cursor: pointer;
    text-transform: capitalize;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  button:hover:not(.active):not(:disabled) {
    color: var(--text);
    background: var(--raised);
  }

  button.active {
    color: var(--accent-text);
    background: var(--accent-fill);
  }

  button:focus-visible {
    outline: none;
    box-shadow: 0 0 0 2px var(--accent-dim);
  }

  .modes.disabled button {
    cursor: default;
    opacity: 0.45;
  }
</style>
