<script lang="ts">
  import { connectBridge, postParam, type EchoParams } from './bridge'
  import {
    DEFAULT_PARAMS,
    FACTORY_PRESETS,
    matchingPresetIndex,
    postAllParams,
  } from './presets'
  import { PARAMS, modeToWire, type Mode, type ParamId } from './params'
  import EchoView from './lib/EchoView.svelte'
  import Knob from './lib/Knob.svelte'
  import ModeSelect from './lib/ModeSelect.svelte'
  import PresetControl from './lib/PresetControl.svelte'
  import Toggle from './lib/Toggle.svelte'
  import logo from './assets/logo.svg'

  /**
   * Local view of the parameters. Rust is the authority: this starts at the
   * schema defaults, is replaced wholesale by `selectInstance`, and is only
   * moved locally to keep a drag responsive — every change is posted in the
   * same tick, and the host's value wins on the next selection.
   */
  let params = $state<EchoParams>({ ...DEFAULT_PARAMS })

  let connected = $state(false)
  let preset = $state<number | null>(0)

  $effect(() =>
    connectBridge(
      (next) => {
        params = next
        preset = matchingPresetIndex(next)
      },
      (isConnected) => {
        connected = isConnected
      },
    ),
  )

  function set(id: ParamId, value: number) {
    params[id] = value
    postParam(id, value)
    preset = null
  }

  function setMode(mode: Mode) {
    params.mode = mode
    postParam('mode', modeToWire(mode))
    preset = null
  }

  function setFlag(id: 'power' | 'freeze', value: boolean) {
    params[id] = value
    postParam(id, value ? 1 : 0)
    if (id === 'freeze') preset = null
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

  // Mono sums the input and reads both rings from the left tap, so the right
  // time and the cross amount have nothing to act on. Disabled rather than
  // hidden: the control still shows the value the DSP will use again on the
  // next mode change.
  const monoCollapsed = $derived(params.mode === 'mono')
</script>

<div class="app">
  <header class="chrome">
    <div class="bar">
      <div class="identity">
        <img class="logo" src={logo} alt="EchoSpace" />
        <div
          class="link"
          class:connected
          title={connected ? 'Linked to DSP' : 'Preview'}
        ></div>
      </div>

      <PresetControl
        {preset}
        names={FACTORY_PRESETS.map((entry) => entry.name)}
        onchange={loadPreset}
        onprevious={() => loadPreset((preset ?? 0) - 1)}
        onnext={() => loadPreset((preset ?? -1) + 1)}
      />

      <div class="bar-right">
        <Toggle
          label="Freeze"
          tone="warn"
          value={params.freeze}
          onchange={(v) => setFlag('freeze', v)}
        />
        <Toggle
          label="Power"
          value={params.power}
          onchange={(v) => setFlag('power', v)}
        />
      </div>
    </div>

    <div class="mode-row">
      <ModeSelect value={params.mode} onchange={setMode} />
    </div>
  </header>

  <div class="body">
    <div class="stage" class:bypassed={!params.power}>
      <EchoView {params} />
    </div>

    <div class="rack">
      <section class="group" aria-label="Timing">
        <div class="group-title">Timing</div>
        <div class="group-knobs">
          <Knob
            spec={PARAMS.timeMsL}
            value={params.timeMsL}
            onchange={(v) => set('timeMsL', v)}
          />
          <Knob
            spec={PARAMS.timeMsR}
            value={params.timeMsR}
            onchange={(v) => set('timeMsR', v)}
            disabled={monoCollapsed}
          />
        </div>
      </section>

      <section class="group" aria-label="Echoes">
        <div class="group-title">Echoes</div>
        <div class="group-knobs">
          <Knob
            spec={PARAMS.feedback}
            value={params.feedback}
            onchange={(v) => set('feedback', v)}
            alert={params.freeze}
            disabled={params.freeze}
          />
          <Knob
            spec={PARAMS.crossFeedback}
            value={params.crossFeedback}
            onchange={(v) => set('crossFeedback', v)}
            disabled={monoCollapsed}
          />
        </div>
      </section>

      <section class="group" aria-label="Tone">
        <div class="group-title">Tone</div>
        <div class="group-knobs">
          <Knob
            spec={PARAMS.lowCutHz}
            value={params.lowCutHz}
            onchange={(v) => set('lowCutHz', v)}
          />
          <Knob
            spec={PARAMS.highCutHz}
            value={params.highCutHz}
            onchange={(v) => set('highCutHz', v)}
          />
          <Knob
            spec={PARAMS.saturation}
            value={params.saturation}
            onchange={(v) => set('saturation', v)}
          />
        </div>
      </section>

      <section class="group output" aria-label="Output">
        <div class="group-title">Output</div>
        <div class="group-knobs">
          <Knob
            spec={PARAMS.mix}
            value={params.mix}
            onchange={(v) => set('mix', v)}
            size="clamp(3.9rem, 6.5vw, 5.1rem)"
          />
          <Knob
            spec={PARAMS.outputDb}
            value={params.outputDb}
            onchange={(v) => set('outputDb', v)}
          />
        </div>
      </section>
    </div>
  </div>
</div>

<style>
  .app {
    display: grid;
    grid-template-rows: auto minmax(0, 1fr);
    height: 100%;
    min-height: 0;
    background: var(--panel);
  }

  .chrome {
    display: grid;
    grid-template-rows: var(--header-height) auto;
    border-bottom: 1px solid var(--border);
    background: linear-gradient(180deg, #13201f 0%, var(--panel) 100%);
  }

  .bar {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr);
    align-items: center;
    gap: var(--space-2) var(--space-3);
    min-width: 0;
    padding: 0 var(--space-3) 0 var(--space-4);
  }

  .identity {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
  }

  .logo {
    display: block;
    width: clamp(7.25rem, 13vw, 10rem);
    max-width: 100%;
    height: auto;
    opacity: 0.95;
  }

  .link {
    flex: none;
    width: 0.4rem;
    height: 0.4rem;
    border-radius: 50%;
    background: var(--track);
    border: 1px solid var(--border-strong);
  }

  .link.connected {
    background: var(--accent);
    border-color: var(--accent);
  }

  .bar-right {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-2);
    min-width: 0;
  }

  .mode-row {
    display: flex;
    justify-content: center;
    min-width: 0;
    padding: 0 var(--space-3) var(--space-2);
  }

  .body {
    display: grid;
    grid-template-rows: minmax(8rem, 1fr) auto;
    gap: var(--space-3);
    min-height: 0;
    padding: var(--space-3);
  }

  .stage {
    display: flex;
    min-height: 0;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--stage-bottom);
    overflow: hidden;
    box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.03);
  }

  .stage.bypassed {
    opacity: 0.38;
  }

  .rack {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--raised);
    overflow: hidden;
  }

  .group {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    min-width: 0;
    padding: var(--space-3) var(--space-2) var(--space-3);
  }

  .group + .group {
    border-left: 1px solid var(--border);
  }

  .group-title {
    padding: 0 var(--space-1);
    color: var(--text-faint);
    font-size: 0.65rem;
    font-weight: 650;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .group-knobs {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(3.85rem, 1fr));
    justify-items: center;
    align-items: start;
    gap: var(--space-2) var(--space-1);
    min-width: 0;
  }

  .output {
    background: color-mix(in srgb, var(--accent) 6%, var(--raised));
  }

  @media (max-width: 980px) {
    .rack {
      grid-template-columns: 1fr 1fr;
    }

    .group:nth-child(3),
    .group:nth-child(4) {
      border-top: 1px solid var(--border);
    }

    .group:nth-child(3) {
      border-left: 0;
    }
  }

  @media (max-width: 760px) {
    .bar {
      grid-template-columns: minmax(0, auto) minmax(0, 1fr) auto;
      padding-inline: var(--space-2);
    }

    .mode-row {
      padding-inline: var(--space-2);
    }

    .body {
      gap: var(--space-2);
      padding: var(--space-2);
    }

    .group {
      padding: var(--space-2);
    }
  }

  @media (max-width: 640px) {
    .rack {
      grid-template-columns: 1fr;
    }

    .group + .group {
      border-left: 0;
      border-top: 1px solid var(--border);
    }

    .group:nth-child(3) {
      border-left: 0;
    }
  }

  @media (max-height: 560px) {
    .body {
      gap: var(--space-2);
      padding: var(--space-2);
    }

    .mode-row {
      padding-bottom: var(--space-1);
    }

    .group {
      padding-block: var(--space-2);
    }
  }
</style>
