<script lang="ts">
  type Props = {
    preset: number | null
    names: string[]
    onchange: (index: number) => void
    onprevious: () => void
    onnext: () => void
  }

  const { preset, names, onchange, onprevious, onnext }: Props = $props()
</script>

<div class="preset">
  <button type="button" class="step" aria-label="Previous preset" onclick={onprevious}>
    ‹
  </button>
  <label class="select">
    <span>{preset === null ? 'Custom' : names[preset]}</span>
    <select
      aria-label="Preset"
      value={preset ?? 'custom'}
      onchange={(event) => {
        const value = (event.currentTarget as HTMLSelectElement).value
        if (value !== 'custom') onchange(Number(value))
      }}
    >
      {#if preset === null}
        <option value="custom">Custom</option>
      {/if}
      {#each names as name, index (name)}
        <option value={index}>{name}</option>
      {/each}
    </select>
  </label>
  <button type="button" class="step" aria-label="Next preset" onclick={onnext}>
    ›
  </button>
</div>

<style>
  .preset {
    display: grid;
    grid-template-columns: 1.6rem minmax(0, 10.5rem) 1.6rem;
    align-items: stretch;
    width: min(100%, 14rem);
    height: 2rem;
    overflow: hidden;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    background: rgba(12, 11, 16, 0.72);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.03),
      0 4px 12px rgba(0, 0, 0, 0.2);
  }

  .step {
    color: var(--text-muted);
    font-size: 1rem;
    cursor: pointer;
  }

  .step:hover {
    color: var(--text);
    background: var(--raised);
  }

  .select {
    position: relative;
    display: grid;
    place-items: center;
    min-width: 0;
    border-inline: 1px solid var(--border);
  }

  .select > span {
    overflow: hidden;
    width: 100%;
    padding: 0 var(--space-2);
    color: var(--text);
    font-size: 0.72rem;
    font-weight: 600;
    text-align: center;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

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
</style>
