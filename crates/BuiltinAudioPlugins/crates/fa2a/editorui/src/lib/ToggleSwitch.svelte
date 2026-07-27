<script lang="ts">
  /**
   * Two-position panel toggle. Used for the COMPRESS/LIMIT mode switch and the
   * meter selector, which are physical switches on the hardware rather than
   * knobs — so they read as switches here too.
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
    gap: 0.2rem;
  }

  .legend {
    color: var(--engrave);
    font-size: 0.54rem;
    font-weight: 700;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    opacity: 0.45;
    text-shadow: 0 1px 0 rgba(255, 250, 240, 0.14);
    white-space: nowrap;
  }

  .legend.on {
    opacity: 1;
    color: var(--readout);
  }

  .body {
    position: relative;
    width: 1.5rem;
    height: 2.4rem;
    border-radius: 0.75rem;
    cursor: pointer;
    background:
      radial-gradient(
        ellipse at 50% 20%,
        rgba(255, 255, 255, 0.14),
        transparent 60%
      ),
      linear-gradient(180deg, #22201c 0%, #100f0d 100%);
    box-shadow:
      inset 0 1px 2px rgba(0, 0, 0, 0.8),
      0 1px 0 rgba(255, 250, 240, 0.12);
  }

  .body:focus-visible {
    outline: none;
    box-shadow:
      inset 0 1px 2px rgba(0, 0, 0, 0.8),
      0 0 0 2px var(--focus);
  }

  .switch.disabled .body {
    cursor: default;
    opacity: 0.4;
  }

  /* The bat travels; the chrome shading stays lit from above so it reads as a
     physical lever rather than a sliding dot. */
  .bat {
    position: absolute;
    left: 50%;
    width: 0.72rem;
    height: 1.15rem;
    margin-left: -0.36rem;
    border-radius: 0.36rem;
    background: linear-gradient(180deg, #e6dcc8 0%, #9c9282 55%, #6b6356 100%);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.6);
    top: 0.16rem;
    transition: top 90ms ease-out;
  }

  .body.down .bat {
    top: 1.09rem;
  }
</style>
