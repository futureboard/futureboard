<script lang="ts">
  /**
   * Two-position panel toggle — Compress/Limit and meter select on the LA-2A.
   */
  type Props = {
    label: string
    /** `[up, down]` legends. */
    options: [string, string]
    /** `false` selects the first option, `true` the second. */
    value: boolean
    onchange: (value: boolean) => void
    disabled?: boolean
  }

  const { label, options, value, onchange, disabled = false }: Props = $props()
</script>

<div class="switch" class:disabled>
  <div class="legend top" class:on={!value}>{options[0]}</div>
  <button
    type="button"
    class="body"
    class:down={value}
    role="switch"
    aria-checked={value}
    aria-label="{label}: {value ? options[1] : options[0]}"
    {disabled}
    onclick={() => onchange(!value)}
    onkeydown={(event) => {
      if (event.key === 'ArrowUp') {
        onchange(false)
        event.preventDefault()
      }
      if (event.key === 'ArrowDown') {
        onchange(true)
        event.preventDefault()
      }
    }}
  >
    <span class="bat"></span>
  </button>
  <div class="legend bottom" class:on={value}>{options[1]}</div>
</div>

<style>
  .switch {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.22rem;
  }

  .legend {
    color: var(--engrave-muted);
    font-size: 0.62rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    white-space: nowrap;
  }

  .legend.on {
    color: var(--engrave);
  }

  .body {
    position: relative;
    width: 1.65rem;
    height: 2.7rem;
    border: 1px solid rgba(0, 0, 0, 0.48);
    border-radius: 0.72rem;
    cursor: pointer;
    background: linear-gradient(180deg, #2b2b29 0%, #111110 100%);
    box-shadow: inset 0 1px 2px rgba(0, 0, 0, 0.55);
  }

  .body:focus-visible {
    outline: none;
    box-shadow:
      inset 0 1px 2px rgba(0, 0, 0, 0.55),
      0 0 0 2px var(--focus);
  }

  .switch.disabled .body {
    cursor: default;
    opacity: 0.4;
  }

  .bat {
    position: absolute;
    left: 50%;
    width: 0.8rem;
    height: 1.25rem;
    margin-left: -0.4rem;
    border-radius: 0.4rem;
    background: linear-gradient(180deg, #f2ead8 0%, #bbb4a5 100%);
    border: 1px solid rgba(0, 0, 0, 0.3);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.28);
    top: 0.16rem;
    transition: top 90ms ease-out;
  }

  .body.down .bat {
    top: 1.22rem;
  }
</style>
