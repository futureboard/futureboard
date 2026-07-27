<script lang="ts">
  import {
    connectBridge,
    postParam,
    type Fa76Params,
    type MeterFrame,
  } from './bridge'
  import {
    DEFAULT_PARAMS,
    FACTORY_PRESETS,
    matchingPresetIndex,
    postAllParams,
  } from './presets'
  import {
    PARAMS,
    RATIOS,
    RATIO_LABELS,
    ratioToWire,
    type ParamId,
    type Ratio,
  } from './params'
  import { linearToDb, type MeterMode } from './meter'
  import AluminumKnob from './lib/AluminumKnob.svelte'
  import BlueVuMeter from './lib/BlueVuMeter.svelte'
  import PresetControl from './lib/PresetControl.svelte'
  import logo from './assets/logo.svg'

  /**
   * Local view of the parameters. Rust is the authority: this starts at the
   * schema defaults, is replaced wholesale by `selectInstance`, and is only
   * moved locally to keep a drag responsive.
   */
  let params = $state<Fa76Params>({ ...DEFAULT_PARAMS })

  let connected = $state(false)
  let preset = $state<number | null>(0)
  let meterMode = $state<MeterMode>('reduction')

  /**
   * Latest telemetry frame. `null` until the host sends one — the meter parks
   * and says "no signal" rather than resting at a number nothing measured.
   */
  let meters = $state<MeterFrame | null>(null)

  $effect(() =>
    connectBridge(
      (next) => {
        params = next
        preset = matchingPresetIndex(next)
      },
      (isConnected) => {
        connected = isConnected
        if (!isConnected) meters = null
      },
      (frame) => {
        meters = frame
      },
    ),
  )

  function set(id: ParamId, value: number) {
    params[id] = value
    postParam(id, value)
    preset = null
  }

  function setRatio(ratio: Ratio) {
    params.ratio = ratio
    postParam('ratio', ratioToWire(ratio))
    preset = null
  }

  function setPower(value: boolean) {
    params.power = value
    postParam('power', value ? 1 : 0)
  }

  function loadPreset(index: number) {
    const wrapped =
      ((index % FACTORY_PRESETS.length) + FACTORY_PRESETS.length) %
      FACTORY_PRESETS.length
    const next = { ...FACTORY_PRESETS[wrapped]!.params }
    params = next
    preset = wrapped
    postAllParams(next)
  }

  // 0 VU is referenced to -18 dBFS, the usual digital alignment, so a track
  // mixed to a conventional level lands the needle near the top of the scale.
  const OUTPUT_REF_DBFS = -18

  const meterValue = $derived.by(() => {
    if (!meters) return 0
    if (meterMode === 'reduction') return meters.gainReductionDb
    return linearToDb(meters.outRms) - OUTPUT_REF_DBFS
  })
</script>

<div class="unit">
  <div class="panel">
    <header class="top">
      <div class="identity">
        <img class="logo" src={logo} alt="FA-76 Limiting Amplifier" />
        <PresetControl
          {preset}
          names={FACTORY_PRESETS.map((entry) => entry.name)}
          onchange={loadPreset}
          onprevious={() => loadPreset((preset ?? 0) - 1)}
          onnext={() => loadPreset((preset ?? -1) + 1)}
        />
      </div>

      <div class="meter-well">
        <BlueVuMeter
          mode={meterMode}
          value={meterValue}
          live={connected && meters !== null}
          clip={meters?.outClip ?? false}
          onclearclip={() => {
            if (meters) meters = { ...meters, outClip: false }
          }}
        />
      </div>

      <div class="side">
        <div class="meter-switch" role="group" aria-label="Meter mode">
          <button
            type="button"
            class:on={meterMode === 'reduction'}
            onclick={() => (meterMode = 'reduction')}
          >GR</button>
          <button
            type="button"
            class:on={meterMode === 'output'}
            onclick={() => (meterMode = 'output')}
          >+4</button>
        </div>

        <button
          type="button"
          class="power"
          class:on={params.power}
          role="switch"
          aria-checked={params.power}
          aria-label="Power"
          onclick={() => setPower(!params.power)}
        >
          <span class="lamp"></span>
          <span class="legend">Power</span>
        </button>
      </div>
    </header>

    <section class="ratios" aria-label="Ratio">
      <span class="bank-label">Ratio</span>
      <div class="bank" role="radiogroup" aria-label="Compression ratio">
        {#each RATIOS as ratio}
          <button
            type="button"
            class="ratio"
            class:on={params.ratio === ratio}
            class:all={ratio === 'all'}
            role="radio"
            aria-checked={params.ratio === ratio}
            onclick={() => setRatio(ratio)}
          >
            {RATIO_LABELS[ratio]}
          </button>
        {/each}
      </div>
    </section>

    <section class="knobs" aria-label="Main controls">
      <AluminumKnob
        spec={PARAMS.inputDb}
        value={params.inputDb}
        onchange={(v) => set('inputDb', v)}
      />
      <AluminumKnob
        spec={PARAMS.outputDb}
        value={params.outputDb}
        onchange={(v) => set('outputDb', v)}
      />
      <AluminumKnob
        spec={PARAMS.attackUs}
        value={params.attackUs}
        onchange={(v) => set('attackUs', v)}
      />
      <AluminumKnob
        spec={PARAMS.releaseMs}
        value={params.releaseMs}
        onchange={(v) => set('releaseMs', v)}
      />
    </section>

    <section class="extras" aria-label="Extras">
      <AluminumKnob
        spec={PARAMS.mix}
        value={params.mix}
        onchange={(v) => set('mix', v)}
        size="var(--knob-sm)"
      />
      <AluminumKnob
        spec={PARAMS.sidechainHpfHz}
        value={params.sidechainHpfHz}
        onchange={(v) => set('sidechainHpfHz', v)}
        size="var(--knob-sm)"
      />
    </section>
  </div>
</div>

<style>
  .unit {
    display: grid;
    place-items: center;
    width: 100%;
    height: 100%;
    padding: var(--space-3);
    background: var(--chassis);
  }

  .panel {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    width: min(100%, 56rem);
    padding: clamp(0.85rem, 2vw, 1.25rem);
    border: 1px solid var(--border);
    border-radius: 0.3rem;
    background:
      linear-gradient(180deg, var(--panel-hi) 0%, var(--panel) 42%, var(--panel-lo) 100%);
    box-shadow:
      inset 0 1px 0 var(--border-hi),
      0 14px 32px rgba(0, 0, 0, 0.5);
  }

  .top {
    display: grid;
    grid-template-columns: minmax(7rem, 11rem) minmax(0, 1fr) auto;
    gap: var(--space-3);
    align-items: center;
  }

  .identity {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    align-items: flex-start;
    min-width: 0;
  }

  .logo {
    width: min(100%, 11rem);
    height: auto;
  }

  .meter-well {
    justify-self: center;
    width: min(100%, 22rem);
    padding: 0.45rem;
    border-radius: 0.2rem;
    background: #0c0e11;
    box-shadow:
      inset 0 2px 5px rgba(0, 0, 0, 0.7),
      0 1px 0 var(--border-hi);
  }

  .side {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    align-items: stretch;
    min-width: 5.5rem;
  }

  .meter-switch {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.2rem;
    padding: 0.2rem;
    border-radius: var(--radius);
    background: var(--inset);
    box-shadow: inset 0 1px 3px rgba(0, 0, 0, 0.55);
  }

  .meter-switch button {
    padding: 0.35rem 0.4rem;
    border-radius: var(--radius-sm);
    color: var(--engrave-muted);
    font-size: 0.68rem;
    font-weight: 750;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .meter-switch button.on {
    color: var(--btn-on-ink);
    background: linear-gradient(180deg, #e8c878, var(--btn-on));
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.35);
  }

  .power {
    display: flex;
    align-items: center;
    gap: 0.45rem;
    padding: 0.45rem 0.55rem;
    border-radius: var(--radius);
    background: linear-gradient(180deg, var(--btn-hi), var(--btn));
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.08),
      0 1px 2px rgba(0, 0, 0, 0.45);
  }

  .lamp {
    width: 0.7rem;
    height: 0.7rem;
    border-radius: 50%;
    background: #2a3140;
    box-shadow: inset 0 1px 2px rgba(0, 0, 0, 0.6);
  }

  .power.on .lamp {
    background: var(--lamp);
    box-shadow: 0 0 6px var(--lamp-soft);
  }

  .legend {
    font-size: 0.68rem;
    font-weight: 750;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    color: var(--engrave-muted);
  }

  .power.on .legend {
    color: var(--engrave);
  }

  .ratios {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: 0.55rem 0.75rem;
    border-radius: 0.2rem;
    background: linear-gradient(180deg, #1a1f26, #12151a);
    box-shadow:
      inset 0 1px 0 var(--border-soft),
      inset 0 -1px 0 rgba(0, 0, 0, 0.4);
  }

  .bank-label {
    color: var(--engrave-muted);
    font-size: 0.68rem;
    font-weight: 750;
    letter-spacing: 0.16em;
    text-transform: uppercase;
  }

  .bank {
    display: grid;
    grid-template-columns: repeat(5, minmax(3.25rem, 1fr));
    gap: 0.35rem;
    flex: 1;
  }

  .ratio {
    min-height: 2.35rem;
    border-radius: 0.15rem;
    border: 1px solid rgba(0, 0, 0, 0.6);
    background: linear-gradient(180deg, #3a424c, #1e2329);
    color: #c8d0da;
    font-size: 0.95rem;
    font-weight: 750;
    letter-spacing: 0.04em;
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.1),
      0 2px 2px rgba(0, 0, 0, 0.35);
  }

  .ratio.on {
    color: var(--btn-on-ink);
    background: linear-gradient(180deg, #f0d48a, #c9a24a);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.4),
      0 1px 2px rgba(0, 0, 0, 0.35);
  }

  .ratio.all.on {
    color: #fff4f0;
    background: linear-gradient(180deg, #d85a45, #a82e22);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.22),
      0 1px 2px rgba(0, 0, 0, 0.4);
  }

  .knobs {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: var(--space-3);
    padding: 0.35rem 0.25rem 0.15rem;
  }

  .extras {
    display: flex;
    justify-content: center;
    gap: clamp(1.5rem, 6vw, 3.5rem);
    padding-top: 0.15rem;
    border-top: 1px solid var(--border-soft);
  }

  @media (max-width: 820px) {
    .top {
      grid-template-columns: 1fr auto;
      grid-template-areas:
        'identity side'
        'meter meter';
    }

    .identity {
      grid-area: identity;
    }

    .meter-well {
      grid-area: meter;
      width: 100%;
      max-width: none;
    }

    .side {
      grid-area: side;
    }

    .knobs {
      grid-template-columns: repeat(2, minmax(0, 1fr));
      row-gap: var(--space-4);
    }
  }
</style>
