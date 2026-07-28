<script lang="ts">
  import {
    connectBridge,
    postParam,
    type BurnLimitParams,
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
    STYLES,
    STYLE_LABELS,
    styleToWire,
    type ParamId,
    type Style,
  } from './params'
  import {
    HISTORY_LENGTH,
    grNorm,
    linearToDb,
    pushHistory,
    shouldTagPeak,
    silentSample,
    type HistorySample,
  } from './meter'
  import GainFader from './lib/GainFader.svelte'
  import LevelMeters from './lib/LevelMeters.svelte'
  import ParamKnob from './lib/ParamKnob.svelte'
  import PresetControl from './lib/PresetControl.svelte'
  import WaveDisplay from './lib/WaveDisplay.svelte'
  import logo from './assets/logo.svg'

  let params = $state<BurnLimitParams>({ ...DEFAULT_PARAMS })
  let connected = $state(false)
  let preset = $state<number | null>(0)
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
              label: `-${sample.grDb.toFixed(1)} dB`,
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

  function setStyle(style: Style) {
    params.style = style
    postParam('style', styleToWire(style))
    preset = null
  }

  function setFlag(id: 'power' | 'truePeak' | 'stereoLink', value: boolean) {
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
</script>

<div class="unit">
  <div class="panel">
    <header class="chrome">
      <div class="brand">
        <img class="logo" src={logo} alt="BurnLimit" />
      </div>
      <PresetControl
        {preset}
        names={FACTORY_PRESETS.map((entry) => entry.name)}
        onchange={loadPreset}
        onprevious={() => loadPreset((preset ?? 0) - 1)}
        onnext={() => loadPreset((preset ?? -1) + 1)}
      />
      <div class="chrome-actions">
        <button
          type="button"
          class="chip"
          class:on={params.truePeak}
          onclick={() => setFlag('truePeak', !params.truePeak)}
        >
          True Peak
        </button>
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
      <div class="gain">
        <GainFader
          spec={PARAMS.gainDb}
          value={params.gainDb}
          onchange={(v) => set('gainDb', v)}
          disabled={!params.power}
        />
      </div>
      <div class="wave">
        <WaveDisplay
          {history}
          ceilingDb={params.ceilingDb}
          live={connected && meters !== null}
          {tags}
        />
      </div>
      <div class="side">
        <LevelMeters
          inPeak={meters?.inPeak ?? 0}
          outPeak={meters?.outPeak ?? 0}
          gainReductionDb={meters?.gainReductionDb ?? 0}
          live={connected && meters !== null}
          clip={meters?.outClip ?? false}
        />
      </div>
    </div>

    <footer class="footer">
      <div class="styles" role="group" aria-label="Limiter style">
        {#each STYLES as style}
          <button
            type="button"
            class="style"
            class:on={params.style === style}
            onclick={() => setStyle(style)}
          >
            {STYLE_LABELS[style]}
          </button>
        {/each}
      </div>

      <div class="params">
        <ParamKnob
          spec={PARAMS.releaseMs}
          value={params.releaseMs}
          onchange={(v) => set('releaseMs', v)}
          disabled={!params.power}
        />
        <ParamKnob
          spec={PARAMS.lookaheadMs}
          value={params.lookaheadMs}
          onchange={(v) => set('lookaheadMs', v)}
          disabled={!params.power}
        />
        <ParamKnob
          spec={PARAMS.mix}
          value={params.mix}
          onchange={(v) => set('mix', v)}
          disabled={!params.power}
        />
        <ParamKnob
          spec={PARAMS.ceilingDb}
          value={params.ceilingDb}
          onchange={(v) => set('ceilingDb', v)}
          disabled={!params.power}
        />
      </div>

      <button
        type="button"
        class="chip"
        class:on={params.stereoLink}
        onclick={() => setFlag('stereoLink', !params.stereoLink)}
      >
        Link
      </button>
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
    max-width: 62rem;
    max-height: 34rem;
    margin: auto;
    padding: var(--s3) var(--s4);
    border: 1px solid var(--border-hi);
    border-radius: calc(var(--r) + 0.15rem);
    background:
      linear-gradient(180deg, rgba(255, 255, 255, 0.03), transparent 28%),
      var(--panel);
    box-shadow: 0 18px 40px rgba(0, 0, 0, 0.35);
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
    width: min(100%, 8.75rem);
    height: auto;
  }

  .chrome-actions {
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: var(--s2);
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
    border-color: rgba(110, 176, 201, 0.5);
    background: rgba(110, 176, 201, 0.14);
  }

  .stage {
    display: grid;
    grid-template-columns: 4.5rem minmax(0, 1fr) 5.75rem;
    min-height: 0;
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: var(--r);
    background: var(--stage);
  }

  .gain {
    border-right: 1px solid var(--border);
    background: linear-gradient(
      180deg,
      rgba(255, 255, 255, 0.03),
      rgba(255, 255, 255, 0.01)
    );
  }

  .wave,
  .side {
    min-width: 0;
    min-height: 0;
  }

  .footer {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--s4);
    min-height: var(--footer-h);
  }

  .styles {
    display: inline-grid;
    grid-auto-flow: column;
    grid-auto-columns: 1fr;
    gap: 0;
    overflow: hidden;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: var(--inset);
  }

  .style {
    min-width: 4.35rem;
    height: 2.125rem;
    padding: 0 0.75rem;
    color: var(--text-muted);
    font-size: 0.68rem;
    font-weight: 650;
    letter-spacing: 0.02em;
    border-right: 1px solid var(--border);
  }

  .style:last-child {
    border-right: 0;
  }

  .style:hover {
    color: var(--text);
  }

  .style.on {
    color: #071018;
    background: var(--steel);
  }

  .params {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    justify-items: center;
    gap: var(--s3);
    min-width: 0;
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

    .footer {
      grid-template-columns: 1fr;
      gap: var(--s3);
      align-items: stretch;
    }

    .params {
      grid-template-columns: 1fr 1fr;
      gap: var(--s3);
    }

    .styles {
      width: 100%;
    }
  }
</style>
