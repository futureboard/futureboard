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
        <span class="wordmark">TRANSIENT</span>
        <span
          class="connection"
          class:live={connected}
          title={connected ? 'Linked to the host' : 'No host instance bound'}
        >
          <i></i>
          {connected ? 'Linked' : 'Standby'}
        </span>
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
          <span class="section-label">Shape</span>
          <div class="knobs">
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
    padding: 0;
    background: var(--bg);
  }

  .panel {
    display: grid;
    grid-template-rows: var(--chrome-h) minmax(0, 1fr) auto;
    gap: 0.65rem;
    width: 100%;
    height: 100%;
    max-width: none;
    max-height: none;
    margin: 0;
    padding: 0.7rem 0.9rem 0.8rem;
    border: 0;
    border-radius: 0;
    background:
      linear-gradient(180deg, rgba(255, 255, 255, 0.035), transparent 24%),
      var(--panel);
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.025);
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
    gap: 0.8rem;
    min-width: 0;
  }

  .wordmark {
    color: var(--text);
    font-size: 0.9rem;
    font-weight: 760;
    letter-spacing: 0.09em;
  }

  .connection {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    color: var(--text-faint);
    font-size: 0.55rem;
    font-weight: 650;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .connection i {
    width: 0.35rem;
    height: 0.35rem;
    border-radius: 50%;
    background: #343b41;
    box-shadow: inset 0 1px 1px rgba(0, 0, 0, 0.75);
  }

  .connection.live {
    color: var(--text-muted);
  }

  .connection.live i {
    background: var(--accent);
    box-shadow: 0 0 0 2px var(--accent-dim);
  }

  .chrome-actions {
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: var(--s2);
  }

  .readout-chip {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.3rem;
    height: var(--chrome-h);
    padding: 0 0.6rem;
    border-radius: 999px;
    border: 1px solid var(--border);
    background: linear-gradient(180deg, #101519, #090d10);
    box-shadow: inset 0 1px 2px rgba(0, 0, 0, 0.5);
  }

  .rlabel {
    color: var(--text-muted);
    font-size: 0.6rem;
    font-weight: 700;
    letter-spacing: 0.08em;
    line-height: 1;
  }

  .rvalue {
    min-width: 2.6ch;
    color: var(--text);
    font-size: 0.78rem;
    font-weight: 700;
    letter-spacing: -0.02em;
    line-height: 1;
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
    line-height: 1;
    background: linear-gradient(180deg, #171d21, #0c1013);
    box-shadow:
      inset 0 1px 0 rgba(255, 255, 255, 0.035),
      0 1px 2px rgba(0, 0, 0, 0.25);
    white-space: nowrap;
  }

  .chip:hover {
    color: var(--text);
    border-color: var(--border-hi);
  }

  .chip.on {
    color: #eafffc;
    border-color: rgba(105, 210, 200, 0.48);
    background: var(--accent-dim);
  }

  .stage {
    position: relative;
    display: grid;
    grid-template-columns: minmax(0, 1fr) 5.75rem;
    min-height: 0;
    overflow: hidden;
    border: 1px solid rgba(0, 0, 0, 0.72);
    border-radius: var(--r);
    background: var(--stage);
    box-shadow:
      inset 0 0 0 1px rgba(255, 255, 255, 0.035),
      inset 0 12px 24px rgba(0, 0, 0, 0.22);
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
    flex-direction: column;
    align-items: stretch;
    gap: 0.55rem;
    padding: 0.7rem 1.1rem 0.85rem;
    border: 1px solid var(--border-hi);
    border-radius: var(--r);
    background: var(--float);
    box-shadow:
      0 16px 32px rgba(0, 0, 0, 0.52),
      inset 0 1px 0 rgba(255, 255, 255, 0.035);
  }

  .section-label {
    color: var(--text-faint);
    font-size: 0.52rem;
    font-weight: 700;
    letter-spacing: 0.16em;
    text-transform: uppercase;
  }

  .knobs {
    display: flex;
    align-items: flex-end;
    gap: 1.35rem;
  }

  .footer {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--s4);
    min-height: var(--footer-h);
    padding: 0 0.15rem;
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
