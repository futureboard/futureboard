<script lang="ts">
  import {
    connectBridge,
    postParam,
    type TransientParams,
    type MeterFrame,
  } from './bridge'
  import {
    DEFAULT_PARAMS,
    FACTORY_PRESETS,
    matchingPresetIndex,
    postAllParams,
  } from './presets'
  import { PARAMS, type ParamId } from './params'
  import {
    HISTORY_LENGTH,
    grNorm,
    linearToDb,
    pushHistory,
    shouldTagPeak,
    silentSample,
    type HistorySample,
  } from './meter'
  import FlatKnob from './lib/FlatKnob.svelte'
  import LevelMeters from './lib/LevelMeters.svelte'
  import PresetControl from './lib/PresetControl.svelte'
  import WaveDisplay from './lib/WaveDisplay.svelte'
  import logo from './assets/logo.svg'

  /**
   * Local view of the parameters. Rust is the authority: this starts at the
   * schema defaults, is replaced wholesale by `selectInstance`, and is only
   * moved locally to keep a drag responsive.
   */
  let params = $state<TransientParams>({ ...DEFAULT_PARAMS })
  let connected = $state(false)
  let preset = $state<number | null>(0)

  /** Latest telemetry frame. `null` until the host sends one. */
  let meters = $state<MeterFrame | null>(null)
  let history = $state<HistorySample[]>(
    Array.from({ length: 48 }, () => silentSample()),
  )
  let tags = $state<{ x: number; y: number; label: string }[]>([])
  let previousGr = 0

  $effect(() =>
    connectBridge(
      (next) => {
        params = next
        preset = matchingPresetIndex(next)
      },
      (isConnected) => {
        connected = isConnected
        if (!isConnected) {
          meters = null
          history = Array.from({ length: 48 }, () => silentSample())
          tags = []
          previousGr = 0
        }
      },
      (frame) => {
        meters = frame
        const sample: HistorySample = {
          inDb: linearToDb(frame.inPeak),
          rmsDb: linearToDb(frame.inRms),
          outDb: linearToDb(frame.outPeak),
          grDb: frame.gainReductionDb,
        }
        history = pushHistory(history, sample, HISTORY_LENGTH)

        if (shouldTagPeak(sample.grDb, previousGr)) {
          const next = [
            ...tags,
            {
              x: 0.92,
              y: grNorm(sample.grDb) * 0.5,
              label: `${sample.grDb.toFixed(1)} dB`,
            },
          ]
          tags = next.slice(-4).map((tag, index, list) => ({
            ...tag,
            x: 0.58 + index * (0.32 / Math.max(list.length, 1)),
          }))
        }
        previousGr = sample.grDb
      },
    ),
  )

  $effect(() => {
    if (!connected) return
    const id = setInterval(() => {
      tags = tags
        .map((tag) => ({ ...tag, x: tag.x - 1 / HISTORY_LENGTH }))
        .filter((tag) => tag.x > 0.05)
    }, 1000 / 30)
    return () => clearInterval(id)
  })

  function set(id: ParamId, value: number) {
    params[id] = value
    postParam(id, value)
    preset = null
  }

  function setFlag(id: 'power' | 'stereoLink', value: boolean) {
    params[id] = value
    postParam(id, value ? 1 : 0)
    if (id !== 'power') preset = null
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

  const shapeReadout = $derived(
    connected && meters ? meters.gainReductionDb.toFixed(1) : '—',
  )
  const outReadout = $derived(
    connected && meters ? linearToDb(meters.outPeak).toFixed(1) : '—',
  )
</script>

<div class="unit">
  <div class="panel">
    <header class="chrome">
      <div class="brand">
        <img class="logo" src={logo} alt="Transient" />
      </div>

      <PresetControl
        {preset}
        names={FACTORY_PRESETS.map((entry) => entry.name)}
        onchange={loadPreset}
        onprevious={() => loadPreset((preset ?? 0) - 1)}
        onnext={() => loadPreset((preset ?? -1) + 1)}
      />

      <div class="chrome-actions">
        <div class="readout-chip">
          <span class="rlabel">SHAPE</span>
          <span class="rvalue shape">{shapeReadout}</span>
        </div>
        <div class="readout-chip">
          <span class="rlabel">OUT</span>
          <span class="rvalue">{outReadout}</span>
        </div>
        <button
          type="button"
          class="chip"
          class:on={!params.power}
          onclick={() => setFlag('power', !params.power)}
        >
          Bypass
        </button>
      </div>
    </header>

    <div class="stage">
      <div class="wave-wrap">
        <WaveDisplay
          {history}
          ceilingDb={0}
          live={connected && meters !== null}
          {tags}
        />

        <div class="control-panel">
          <div class="knobs">
            <svg class="curve" viewBox="0 0 240 62" aria-hidden="true">
              <path
                d="M16 44 C 48 12, 72 12, 100 28 C 128 44, 160 52, 224 36"
              />
            </svg>
            <FlatKnob
              spec={PARAMS.attack}
              value={params.attack}
              onchange={(v) => set('attack', v)}
              size="var(--knob-lg)"
              disabled={!params.power}
            />
            <FlatKnob
              spec={PARAMS.sustain}
              value={params.sustain}
              onchange={(v) => set('sustain', v)}
              size="var(--knob-lg)"
              disabled={!params.power}
            />
            <FlatKnob
              spec={PARAMS.speed}
              value={params.speed}
              onchange={(v) => set('speed', v)}
              size="var(--knob-md)"
              disabled={!params.power}
            />
          </div>
        </div>
      </div>

      <div class="side">
        <LevelMeters
          inPeak={meters?.inPeak ?? 0}
          outPeak={meters?.outPeak ?? 0}
          gainReductionDb={meters?.gainReductionDb ?? 0}
          live={connected && meters !== null}
          inClip={meters?.inClip ?? false}
          outClip={meters?.outClip ?? false}
        />
      </div>
    </div>

    <footer class="footer">
      <FlatKnob
        spec={PARAMS.mix}
        value={params.mix}
        onchange={(v) => set('mix', v)}
        size="var(--knob-sm)"
        disabled={!params.power}
      />

      <div class="toggles">
        <button
          type="button"
          class="chip"
          class:on={params.stereoLink}
          onclick={() => setFlag('stereoLink', !params.stereoLink)}
        >
          Stereo Link
        </button>
      </div>
    </footer>
  </div>
</div>

<style>
  .unit {
    display: grid;
    width: 100%;
    height: 100%;
    padding: var(--s3);
    background: var(--bg);
  }

  .panel {
    display: grid;
    grid-template-rows: var(--chrome-h) minmax(0, 1fr) auto;
    gap: var(--s3);
    width: 100%;
    height: 100%;
    max-width: 68rem;
    max-height: 36rem;
    margin: auto;
    padding: var(--s3) var(--s4);
    border: 1px solid var(--border-hi);
    border-radius: calc(var(--r) + 0.15rem);
    background:
      linear-gradient(180deg, rgba(255, 255, 255, 0.025), transparent 28%),
      var(--panel);
    box-shadow: 0 18px 40px rgba(0, 0, 0, 0.4);
  }

  .chrome {
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    gap: var(--s3);
    min-height: var(--chrome-h);
  }

  .brand {
    display: flex;
    align-items: center;
    min-width: 0;
  }

  .logo {
    width: min(100%, 10.5rem);
    height: auto;
  }

  .chrome-actions {
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: var(--s2);
  }

  .readout-chip {
    display: flex;
    align-items: baseline;
    gap: 0.3rem;
    height: var(--chrome-h);
    padding: 0 0.6rem;
    border-radius: 999px;
    border: 1px solid var(--border);
    background: var(--inset);
  }

  .rlabel {
    color: var(--text-muted);
    font-size: 0.6rem;
    font-weight: 700;
    letter-spacing: 0.08em;
  }

  .rvalue {
    min-width: 2.6ch;
    color: var(--text);
    font-size: 0.78rem;
    font-weight: 700;
    letter-spacing: -0.02em;
    text-align: right;
  }

  .rvalue.shape {
    color: var(--amber);
  }

  .chip {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    height: var(--chrome-h);
    padding: 0 0.75rem;
    border-radius: 999px;
    border: 1px solid var(--border);
    color: var(--text-muted);
    font-size: 0.65rem;
    font-weight: 650;
    letter-spacing: 0.04em;
    background: var(--inset);
    white-space: nowrap;
  }

  .chip:hover {
    color: var(--text);
    border-color: var(--border-hi);
  }

  .chip.on {
    color: var(--text);
    border-color: rgba(46, 196, 182, 0.5);
    background: var(--accent-dim);
  }

  .stage {
    position: relative;
    display: grid;
    grid-template-columns: minmax(0, 1fr) 5.75rem;
    min-height: 0;
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--stage);
  }

  .wave-wrap {
    position: relative;
    min-width: 0;
    min-height: 0;
  }

  .side {
    min-width: 0;
    min-height: 0;
  }

  .control-panel {
    position: absolute;
    bottom: var(--s3);
    left: var(--s3);
    z-index: 3;
    display: flex;
    align-items: center;
    gap: var(--s4);
    padding: 0.75rem 1.1rem;
    border: 1px solid var(--border-hi);
    border-radius: var(--r);
    background: var(--float);
    box-shadow: 0 14px 30px rgba(0, 0, 0, 0.5);
    backdrop-filter: blur(8px);
  }

  .knobs {
    position: relative;
    display: flex;
    align-items: flex-end;
    gap: 1.35rem;
  }

  .curve {
    position: absolute;
    top: -0.35rem;
    left: 0;
    z-index: 0;
    width: 100%;
    height: 3.4rem;
    pointer-events: none;
  }

  .curve path {
    fill: none;
    stroke: rgba(46, 196, 182, 0.32);
    stroke-width: 1.5;
    stroke-linecap: round;
  }

  .footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s4);
    min-height: var(--footer-h);
  }

  .toggles {
    display: flex;
    align-items: center;
    gap: var(--s2);
  }

  @media (max-width: 960px) {
    .panel {
      max-height: none;
    }

    .chrome {
      grid-template-columns: 1fr;
      justify-items: start;
      gap: var(--s2);
    }

    .chrome-actions {
      justify-content: flex-start;
    }

    .stage {
      grid-template-columns: 1fr;
    }

    .side {
      display: none;
    }

    .control-panel {
      flex-wrap: wrap;
      right: var(--s3);
    }

    .footer {
      flex-direction: column;
      align-items: stretch;
    }
  }
</style>
