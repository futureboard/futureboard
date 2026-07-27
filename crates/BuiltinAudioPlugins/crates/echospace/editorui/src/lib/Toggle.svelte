<script lang="ts">
  type Props = {
    label: string
    value: boolean
    onchange: (value: boolean) => void
    /** `warn` marks a hold/override state; `accent` marks ordinary engagement. */
    tone?: 'accent' | 'warn'
    disabled?: boolean
  }

  const {
    label,
    value,
    onchange,
    tone = 'accent',
    disabled = false,
  }: Props = $props()
</script>

<button
  type="button"
  class="toggle {tone}"
  class:active={value}
  role="switch"
  aria-checked={value}
  aria-label={label}
  {disabled}
  onclick={() => onchange(!value)}
>
  <span class="dot"></span>
  <span class="text">{label}</span>
</button>

<style>
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: 0.45rem;
    min-height: 1.85rem;
    padding: 0.35rem 0.85rem 0.35rem 0.7rem;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--raised);
    color: var(--text-muted);
    font: inherit;
    font-size: 0.78rem;
    font-weight: 600;
    cursor: pointer;
  }

  .toggle:hover:not(:disabled) {
    color: var(--text);
    border-color: var(--border-strong);
  }

  .toggle:focus-visible {
    outline: none;
    box-shadow: 0 0 0 2px var(--accent-dim);
  }

  .toggle:disabled {
    cursor: default;
    opacity: 0.45;
  }

  .dot {
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 50%;
    background: var(--track);
    border: 1px solid var(--border-strong);
  }

  .toggle.active {
    color: var(--text);
    border-color: var(--accent-dim);
  }

  .toggle.accent.active .dot {
    background: var(--accent);
    border-color: var(--accent);
  }

  .toggle.warn.active {
    border-color: var(--warn-dim);
    color: var(--warn);
  }

  .toggle.warn.active .dot {
    background: var(--warn);
    border-color: var(--warn);
  }
</style>
