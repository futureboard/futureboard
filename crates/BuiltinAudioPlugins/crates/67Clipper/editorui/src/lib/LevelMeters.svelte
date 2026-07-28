<script lang="ts">
  import { advance, grNorm, levelNorm, linearToDb } from '../meter'
  import { untrack } from 'svelte'

  type Props = {
    inPeak: number
    outPeak: number
    gainReductionDb: number
    live: boolean
    inClip: boolean
    outClip: boolean
  }

  const { inPeak, outPeak, gainReductionDb, live, inClip, outClip }: Props =
    $props()

  const inTarget = $derived(live ? 1 - levelNorm(linearToDb(inPeak)) : 0)
  const outTarget = $derived(live ? 1 - levelNorm(linearToDb(outPeak)) : 0)
  const grTarget = $derived(live ? grNorm(gainReductionDb) : 0)

  let inBar = $state(untrack(() => inTarget))
  let outBar = $state(untrack(() => outTarget))
  let grBar = $state(untrack(() => grTarget))

  $effect(() => {
    let raf = 0
    let last = performance.now()
    const step = (now: number) => {
      const dt = Math.min((now - last) / 1000, 0.25)
      last = now
      inBar = advance(inBar, inTarget, dt)
      outBar = advance(outBar, outTarget, dt)
      grBar = advance(grBar, grTarget, dt, 0.05)
      raf = requestAnimationFrame(step)
    }
    raf = requestAnimationFrame(step)
    return () => cancelAnimationFrame(raf)
  })

  const grDb = $derived(live ? gainReductionDb : null)
</script>

<aside class="meters" aria-label="Level meters">
  <div class="bars">
    <div class="col">
      <div class="bar" role="img" aria-label="Input">
        <div class="fill in" style="height: {(inBar * 100).toFixed(2)}%"></div>
        <div class="clip-led" class:on={live && inClip}></div>
      </div>
      <span class="label">In</span>
    </div>
    <div class="col">
      <div class="bar" role="img" aria-label="Output">
        <div class="fill out" style="height: {(outBar * 100).toFixed(2)}%"></div>
        <div class="clip-led" class:on={live && outClip}></div>
      </div>
      <span class="label">Out</span>
    </div>
    <div class="col gr">
      <div class="bar" role="img" aria-label="Gain reduction">
        <div class="fill gr" style="height: {(grBar * 100).toFixed(2)}%"></div>
      </div>
      <span class="label">GR</span>
    </div>
  </div>

  <div class="readout">
    <span class="num">{grDb === null ? '—' : `-${grDb.toFixed(1)}`}</span>
    <span class="unit">dB</span>
  </div>
</aside>

<style>
  .meters {
    display: flex;
    flex-direction: column;
    gap: var(--s3);
    height: 100%;
    padding: var(--s3) var(--s2) var(--s2);
    background: rgba(255, 255, 255, 0.02);
    border-left: 1px solid var(--border);
  }

  .bars {
    display: grid;
    grid-template-columns: 1fr 1fr 1.15fr;
    gap: 0.35rem;
    flex: 1;
    min-height: 0;
  }

  .col {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.35rem;
    min-height: 0;
  }

  .bar {
    position: relative;
    flex: 1;
    width: 100%;
    min-height: 0;
    overflow: hidden;
    border-radius: var(--r-sm);
    background: rgba(255, 255, 255, 0.04);
  }

  .fill {
    position: absolute;
    right: 0;
    bottom: 0;
    left: 0;
    border-radius: inherit;
  }

  .fill.in {
    background: #2f6ea8;
  }

  .fill.out {
    background: var(--accent);
  }

  .fill.gr {
    background: var(--red);
  }

  .clip-led {
    position: absolute;
    top: 0.25rem;
    left: 50%;
    width: 0.3rem;
    height: 0.3rem;
    transform: translateX(-50%);
    border-radius: 50%;
    background: #2a3038;
  }

  .clip-led.on {
    background: var(--clip);
  }

  .label {
    color: var(--text-muted);
    font-size: 0.5rem;
    font-weight: 650;
    letter-spacing: 0.1em;
    text-transform: uppercase;
  }

  .readout {
    display: flex;
    align-items: baseline;
    justify-content: center;
    gap: 0.2rem;
    padding-top: var(--s1);
    border-top: 1px solid var(--border);
  }

  .num {
    color: var(--red);
    font-size: 1rem;
    font-weight: 700;
    letter-spacing: -0.03em;
  }

  .unit {
    color: var(--text-muted);
    font-size: 0.55rem;
    font-weight: 650;
  }
</style>
