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
    <div class="top">
      <div class="badge">
        <div class="maker">Futureboard</div>
        <div class="model">FA-2A</div>
        <div class="kind">Leveling Amplifier</div>
      </div>

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
          options={['GR', 'Output']}
          value={meterMode === 'output'}
          onchange={(v) => (meterMode = v ? 'output' : 'reduction')}
        />
        <ToggleSwitch
          label="Mode"
          options={['Comp', 'Limit']}
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

    <div class="rail"></div>

    <div class="controls">
      <div class="mains">
        <BakeliteKnob
          spec={PARAMS.peakReduction}
          value={params.peakReduction}
          onchange={(v) => set('peakReduction', v)}
          dial
        />
        <BakeliteKnob
          spec={PARAMS.gainDb}
          value={params.gainDb}
          onchange={(v) => set('gainDb', v)}
          dial
        />
      </div>

      <div class="trim">
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
      </div>
    </div>

    <div class="feet">
      <span class="screw"></span>
      <span class="link" class:connected></span>
      <span class="screw"></span>
    </div>
  </div>
</div>

<style>
  .unit {
    display: flex;
    align-items: stretch;
    height: 100%;
    padding: clamp(0.4rem, 1.4vw, 1rem);
    background:
      radial-gradient(
        ellipse at 50% 0%,
        rgba(255, 190, 120, 0.06),
        transparent 65%
      ),
      var(--panel-edge);
  }

  /* Brushed enamel: a vertical light falloff plus a fine horizontal grain. */
  .panel {
    position: relative;
    display: grid;
    grid-template-rows: minmax(0, 1fr) auto minmax(0, 1fr) auto;
    flex: 1;
    min-width: 0;
    padding: clamp(0.5rem, 1.6vw, 1.1rem);
    border-radius: var(--radius);
    background:
      repeating-linear-gradient(
        180deg,
        rgba(255, 255, 255, 0.022) 0px,
        rgba(255, 255, 255, 0.022) 1px,
        rgba(0, 0, 0, 0.022) 1px,
        rgba(0, 0, 0, 0.022) 2px
      ),
      linear-gradient(
        180deg,
        var(--panel-hi) 0%,
        var(--panel) 46%,
        var(--panel-lo) 100%
      );
    box-shadow:
      inset 0 1px 0 rgba(255, 245, 225, 0.22),
      inset 0 -1px 0 rgba(0, 0, 0, 0.35),
      0 2px 10px rgba(0, 0, 0, 0.55);
  }

  .top {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr);
    align-items: center;
    gap: clamp(0.5rem, 2vw, 1.6rem);
    min-height: 0;
  }

  .badge {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
    min-width: 0;
  }

  .maker {
    color: var(--engrave);
    font-size: 0.58rem;
    font-weight: 700;
    letter-spacing: 0.22em;
    text-transform: uppercase;
    opacity: 0.72;
    text-shadow: 0 1px 0 rgba(255, 250, 240, 0.16);
  }

  .model {
    color: var(--readout);
    font-size: clamp(1.15rem, 3vw, 1.75rem);
    font-weight: 800;
    letter-spacing: 0.06em;
    line-height: 1;
    text-shadow:
      0 1px 0 rgba(255, 250, 240, 0.2),
      0 -1px 0 rgba(0, 0, 0, 0.4);
  }

  .kind {
    color: var(--engrave);
    font-size: 0.56rem;
    font-weight: 600;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    opacity: 0.6;
  }

  /* Recessed bezel the meter sits down inside. */
  .meter-well {
    width: clamp(12rem, 34vw, 20rem);
    padding: 0.4rem;
    border-radius: 6px;
    background: linear-gradient(180deg, var(--inset-lo), var(--inset));
    box-shadow:
      inset 0 2px 5px rgba(0, 0, 0, 0.6),
      0 1px 0 rgba(255, 250, 240, 0.14);
  }

  .switches {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: clamp(0.6rem, 2.2vw, 1.5rem);
    min-width: 0;
  }

  .power {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.3rem;
    cursor: pointer;
  }

  .lamp {
    width: 0.85rem;
    height: 0.85rem;
    border-radius: 50%;
    background: radial-gradient(circle at 35% 30%, #6a5f4e, #241f19);
    box-shadow: inset 0 1px 2px rgba(0, 0, 0, 0.7);
  }

  .power.on .lamp {
    background: radial-gradient(circle at 35% 30%, #ffe3ae, var(--lamp) 55%, #a35a12);
    box-shadow:
      inset 0 1px 2px rgba(120, 60, 10, 0.5),
      0 0 10px rgba(255, 170, 70, 0.75);
  }

  .legend {
    color: var(--engrave);
    font-size: 0.54rem;
    font-weight: 700;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    text-shadow: 0 1px 0 rgba(255, 250, 240, 0.14);
  }

  /* Engraved separator between the meter bay and the control row. */
  .rail {
    height: 2px;
    margin: clamp(0.4rem, 1.4vh, 0.9rem) 0;
    border-radius: 1px;
    background: linear-gradient(
      180deg,
      rgba(0, 0, 0, 0.35),
      rgba(255, 250, 240, 0.14)
    );
  }

  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-evenly;
    gap: clamp(0.5rem, 2vw, 1.6rem);
    min-width: 0;
    min-height: 0;
  }

  /* The two hero controls hold their own width; the trim bank takes what is
     left. Without this the growing bank squeezed them together. */
  .mains {
    display: flex;
    align-items: center;
    justify-content: space-evenly;
    gap: clamp(1rem, 4vw, 3.5rem);
    flex: 1 1 auto;
    min-width: 0;
  }

  /* The five trim controls read as one secondary bank, set off from the two
     hero knobs by a scribed line rather than by a gap. */
  .trim {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(3.6rem, 1fr));
    justify-items: center;
    align-items: start;
    gap: clamp(0.4rem, 1.4vh, 0.8rem) clamp(0.3rem, 1.2vw, 0.9rem);
    flex: 1 1 16rem;
    min-width: 0;
    padding-left: clamp(0.5rem, 2vw, 1.5rem);
    border-left: 1px solid rgba(0, 0, 0, 0.28);
    box-shadow: inset 1px 0 0 rgba(255, 250, 240, 0.1);
  }

  .feet {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-top: clamp(0.3rem, 1vh, 0.6rem);
  }

  /* Rack screws, and a small pilot between them that lights when the editor
     is bound to a running instance. */
  .screw {
    width: 0.6rem;
    height: 0.6rem;
    border-radius: 50%;
    background: radial-gradient(circle at 34% 30%, #b4a992, #4c463c);
    box-shadow:
      inset 0 -1px 1px rgba(0, 0, 0, 0.5),
      0 1px 0 rgba(255, 250, 240, 0.12);
  }

  .link {
    width: 0.34rem;
    height: 0.34rem;
    border-radius: 50%;
    background: #2c2822;
    box-shadow: inset 0 1px 1px rgba(0, 0, 0, 0.7);
  }

  .link.connected {
    background: #7fd6a0;
    box-shadow: 0 0 6px rgba(127, 214, 160, 0.7);
  }

  @media (max-width: 820px) {
    .top {
      grid-template-columns: minmax(0, 1fr) auto;
      row-gap: 0.6rem;
    }

    .badge {
      grid-column: 1;
    }

    .meter-well {
      grid-column: 2;
      grid-row: 1;
    }

    .switches {
      grid-column: 1 / -1;
      justify-content: center;
    }

    .controls {
      flex-wrap: wrap;
      justify-content: center;
    }
  }
</style>
