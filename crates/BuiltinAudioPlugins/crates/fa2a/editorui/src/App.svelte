<script lang="ts">
  import {
    connectBridge,
    postParam,
    type Fa2aParams,
    type MeterFrame,
  } from './bridge'
  import { PARAMS, modeToWire, type Mode, type ParamId } from './params'
  import { linearToDb, type MeterMode } from './meter'
  import BakeliteKnob from './lib/BakeliteKnob.svelte'
  import ToggleSwitch from './lib/ToggleSwitch.svelte'
  import VuMeter from './lib/VuMeter.svelte'
  import logo from './assets/logo.svg'

  /**
   * Local view of the parameters. Rust is the authority: this starts at the
   * schema defaults, is replaced wholesale by `selectInstance`, and is only
   * moved locally to keep a drag responsive.
   */
  let params = $state<Fa2aParams>({
    power: true,
    mode: 'compress',
    peakReduction: PARAMS.peakReduction.default,
    gainDb: PARAMS.gainDb.default,
    emphasis: PARAMS.emphasis.default,
    mix: PARAMS.mix.default,
    color: PARAMS.color.default,
    sidechainLowCutHz: PARAMS.sidechainLowCutHz.default,
    outputTrimDb: PARAMS.outputTrimDb.default,
  })

  let connected = $state(false)
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
  }

  function setMode(mode: Mode) {
    params.mode = mode
    postParam('mode', modeToWire(mode))
  }

  function setPower(value: boolean) {
    params.power = value
    postParam('power', value ? 1 : 0)
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
    <section class="face" aria-label="Main controls">
      <div class="hero-control">
        <BakeliteKnob
          spec={PARAMS.gainDb}
          value={params.gainDb}
          onchange={(v) => set('gainDb', v)}
          dial
        />
      </div>

      <div class="center-bay">
        <header class="badge">
          <img class="logo" src={logo} alt="FA-2A Leveling Amplifier" />
        </header>

        <div class="meter-well">
          <VuMeter
            mode={meterMode}
            value={meterValue}
            live={connected && meters !== null}
            clip={meters?.outClip ?? false}
            onclearclip={() => {
              if (meters) meters = { ...meters, outClip: false }
            }}
          />
        </div>

        <div class="switches">
          <ToggleSwitch
            label="Meter"
            options={['GR', '+4']}
            value={meterMode === 'output'}
            onchange={(v) => (meterMode = v ? 'output' : 'reduction')}
          />
          <ToggleSwitch
            label="Mode"
            options={['Compress', 'Limit']}
            value={params.mode === 'limit'}
            onchange={(v) => setMode(v ? 'limit' : 'compress')}
          />
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
      </div>

      <div class="hero-control">
        <BakeliteKnob
          spec={PARAMS.peakReduction}
          value={params.peakReduction}
          onchange={(v) => set('peakReduction', v)}
          dial
        />
      </div>
    </section>

    <section class="extras" aria-label="Extras">
      <BakeliteKnob
        spec={PARAMS.emphasis}
        value={params.emphasis}
        onchange={(v) => set('emphasis', v)}
        size="var(--knob-sm)"
      />
      <BakeliteKnob
        spec={PARAMS.sidechainLowCutHz}
        value={params.sidechainLowCutHz}
        onchange={(v) => set('sidechainLowCutHz', v)}
        size="var(--knob-sm)"
      />
      <BakeliteKnob
        spec={PARAMS.color}
        value={params.color}
        onchange={(v) => set('color', v)}
        size="var(--knob-sm)"
      />
      <BakeliteKnob
        spec={PARAMS.mix}
        value={params.mix}
        onchange={(v) => set('mix', v)}
        size="var(--knob-sm)"
      />
      <BakeliteKnob
        spec={PARAMS.outputTrimDb}
        value={params.outputTrimDb}
        onchange={(v) => set('outputTrimDb', v)}
        size="var(--knob-sm)"
      />
    </section>

    <footer class="feet">
      <span
        class="link"
        class:connected
        title={connected ? 'Linked to DSP' : 'Preview'}
      ></span>
    </footer>
  </div>
</div>

<style>
  .unit {
    display: flex;
    height: 100%;
    padding: 0.5rem;
    background: var(--chassis);
  }

  .panel {
    display: grid;
    grid-template-rows: minmax(0, 1fr) auto auto;
    flex: 1;
    min-width: 0;
    min-height: 0;
    padding: 0.85rem 1.1rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: linear-gradient(
      180deg,
      var(--panel-hi) 0%,
      var(--panel) 38%,
      var(--panel-lo) 100%
    );
    box-shadow:
      inset 0 1px 0 var(--border-hi),
      inset 0 -1px 0 rgba(0, 0, 0, 0.22),
      0 8px 22px rgba(0, 0, 0, 0.35);
  }

  .face {
    display: grid;
    grid-template-columns:
      minmax(10rem, 0.9fr)
      minmax(18.5rem, 1.55fr)
      minmax(10rem, 0.9fr);
    align-items: center;
    gap: clamp(1rem, 3vw, 2.4rem);
    min-width: 0;
    min-height: 0;
    padding: 0.3rem 0.4rem 0.85rem;
  }

  .hero-control {
    display: grid;
    place-items: center;
    min-width: 0;
  }

  .center-bay {
    display: grid;
    grid-template-rows: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: 0.65rem;
    min-width: 0;
    min-height: 0;
  }

  .badge {
    display: flex;
    align-items: center;
    justify-content: center;
    min-width: 0;
  }

  .logo {
    display: block;
    width: min(100%, 23rem);
    height: auto;
  }

  .meter-well {
    width: 100%;
    max-width: 25.5rem;
    justify-self: center;
    padding: 0.55rem;
    border: 1px solid rgba(0, 0, 0, 0.45);
    border-radius: 3px;
    background: linear-gradient(180deg, var(--bezel) 0%, #1a1814 100%);
    box-shadow:
      inset 0 1px 0 rgba(255, 248, 235, 0.08),
      0 1px 0 var(--border-soft),
      0 4px 10px rgba(0, 0, 0, 0.28);
  }

  .switches {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: clamp(1.25rem, 3vw, 2rem);
    min-width: 0;
  }

  .power {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.28rem;
    cursor: pointer;
  }

  .lamp {
    width: 0.92rem;
    height: 0.92rem;
    border-radius: 50%;
    border: 1px solid rgba(0, 0, 0, 0.5);
    background: radial-gradient(circle at 35% 30%, #3f382e, #17140f);
  }

  .power.on .lamp {
    background: radial-gradient(circle at 35% 28%, #ffe7b5, var(--lamp) 68%);
    border-color: color-mix(in srgb, var(--lamp) 40%, #000);
    box-shadow: 0 0 9px var(--lamp-soft);
  }

  .legend {
    color: var(--engrave-muted);
    font-size: 0.62rem;
    font-weight: 700;
    letter-spacing: 0.12em;
    text-transform: uppercase;
  }

  .power.on .legend {
    color: var(--engrave);
  }

  .extras {
    display: grid;
    grid-template-columns: repeat(5, minmax(0, 1fr));
    justify-items: center;
    gap: 0.65rem;
    min-width: 0;
    padding: 0.7rem 0.5rem 0.15rem;
    border-top: 1px solid rgba(0, 0, 0, 0.2);
    box-shadow: inset 0 1px 0 var(--border-soft);
    opacity: 0.86;
  }

  .feet {
    display: flex;
    justify-content: center;
    padding-top: 0.25rem;
  }

  .link {
    width: 0.34rem;
    height: 0.34rem;
    border-radius: 50%;
    background: #2a2621;
    border: 1px solid rgba(0, 0, 0, 0.4);
  }

  .link.connected {
    background: var(--link);
    border-color: color-mix(in srgb, var(--link) 45%, #000);
  }

  @media (max-width: 820px) {
    .face {
      grid-template-columns:
        minmax(7.5rem, 0.8fr)
        minmax(14rem, 1.35fr)
        minmax(7.5rem, 0.8fr);
      gap: 0.6rem;
    }

    .extras {
      grid-template-columns: repeat(auto-fit, minmax(3.2rem, 1fr));
    }
  }

  @media (max-height: 520px) {
    .panel {
      padding-block: 0.5rem 0.35rem;
    }

    .face {
      padding-bottom: 0.4rem;
    }

    .center-bay {
      gap: 0.3rem;
    }

    .extras {
      padding-top: 0.4rem;
    }
  }
</style>
