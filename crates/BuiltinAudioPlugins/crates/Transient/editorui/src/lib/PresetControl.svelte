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
    grid-template-columns: 1.75rem minmax(0, 1fr) 1.75rem;
    align-items: stretch;
    width: min(100%, 13.5rem);
    height: var(--chrome-h);
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--inset);
  }

  .step {
    color: var(--text-muted);
    font-size: 1rem;
    line-height: 1;
  }

  .step:hover {
    color: var(--text);
    background: rgba(255, 255, 255, 0.04);
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
    padding: 0 var(--s2);
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
  }
</style>
