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
    grid-template-columns: 1.55rem minmax(0, 1fr) 1.55rem;
    align-items: stretch;
    width: 100%;
    max-width: 11rem;
    height: 1.85rem;
    overflow: hidden;
    border: 1px solid rgba(0, 0, 0, 0.55);
    border-radius: var(--radius);
    background: linear-gradient(180deg, #2a3038, #161a20);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.08),
      0 1px 2px rgba(0, 0, 0, 0.35);
  }

  .step {
    color: var(--engrave-muted);
    font-size: 0.95rem;
    cursor: pointer;
  }

  .step:hover {
    color: var(--engrave);
    background: rgba(255, 255, 255, 0.04);
  }

  .select {
    position: relative;
    display: grid;
    place-items: center;
    min-width: 0;
    border-inline: 1px solid rgba(0, 0, 0, 0.4);
  }

  .select > span {
    overflow: hidden;
    width: 100%;
    padding: 0 0.35rem;
    color: var(--engrave);
    font-size: 0.68rem;
    font-weight: 650;
    letter-spacing: 0.02em;
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
    background: #1a1e24;
    color: var(--engrave);
  }
</style>
