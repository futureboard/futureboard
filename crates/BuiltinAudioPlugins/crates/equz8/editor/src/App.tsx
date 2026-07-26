import { useCallback, useEffect, useState } from 'react'
import { connectBridge, postParam, type Band, type EqParams } from './bridge'
import { BandChips } from './components/BandChips'
import { ControlRack } from './components/ControlRack'
import { Header } from './components/Header'
import { ResponseGraph } from './components/ResponseGraph'
import {
  DEFAULT_PARAMS,
  FACTORY_PRESETS,
  cloneParams,
  matchingPresetIndex,
  postAllParams,
  postBandPatch,
} from './lib/presets'
import './Editor.scss'

function App() {
  const [params, setParams] = useState<EqParams>(DEFAULT_PARAMS)
  const [selected, setSelected] = useState(2)
  const [connected, setConnected] = useState(false)
  const [showBandCurves, setShowBandCurves] = useState(true)
  const [preset, setPreset] = useState<number | null>(0)

  useEffect(
    () =>
      connectBridge(
        (nativeParams) => {
          setParams(nativeParams)
          setPreset(matchingPresetIndex(nativeParams))
        },
        setConnected,
      ),
    [],
  )

  const updateBand = useCallback((index: number, patch: Partial<Band>) => {
    setParams((current) => ({
      ...current,
      bands: current.bands.map((band, bandIndex) =>
        bandIndex === index ? { ...band, ...patch } : band,
      ),
    }))
    setPreset(null)
    postBandPatch(index, patch)
  }, [])

  const updateGlobal = useCallback(
    (patch: Partial<Pick<EqParams, 'power' | 'outputDb' | 'mix'>>) => {
      setParams((current) => ({ ...current, ...patch }))
      setPreset(null)
      if (patch.power !== undefined) postParam('power', patch.power ? 1 : 0)
      if (patch.outputDb !== undefined) postParam('outputDb', patch.outputDb)
      if (patch.mix !== undefined) postParam('mix', patch.mix)
    },
    [],
  )

  const loadPreset = useCallback((index: number) => {
    const wrapped = (index + FACTORY_PRESETS.length) % FACTORY_PRESETS.length
    const entry = FACTORY_PRESETS[wrapped]
    if (!entry) return
    const next = cloneParams(entry.params)
    setParams(next)
    setPreset(wrapped)
    postAllParams(next)
  }, [])

  const band = params.bands[selected]
  if (!band) return null

  const defaults = DEFAULT_PARAMS.bands[selected]
  if (!defaults) return null

  return (
    <main className={`editor${params.power ? '' : ' is-bypassed'}`}>
      <Header
        connected={connected}
        power={params.power}
        preset={preset}
        presetNames={FACTORY_PRESETS.map((entry) => entry.name)}
        onPresetChange={loadPreset}
        onPreviousPreset={() => loadPreset((preset ?? 0) - 1)}
        onNextPreset={() => loadPreset((preset ?? -1) + 1)}
        onReset={() => loadPreset(0)}
        onPowerChange={(power) => updateGlobal({ power })}
      />

      <section className="stage">
        <ResponseGraph
          bands={params.bands}
          selected={selected}
          bypassed={!params.power}
          showBandCurves={showBandCurves}
          onSelect={setSelected}
          onBandChange={updateBand}
        />

        <div className="stage-overlay">
          <BandChips
            bands={params.bands}
            selected={selected}
            onSelect={setSelected}
            onToggle={(index) =>
              updateBand(index, { active: !params.bands[index]?.active })
            }
          />
          <button
            type="button"
            className={`graph-option${showBandCurves ? ' is-on' : ''}`}
            aria-pressed={showBandCurves}
            title="Show each band's curve"
            aria-label="Show each band's curve"
            onClick={() => setShowBandCurves((value) => !value)}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M2 17c5 0 4-9 9-9s4 6 5 6 3-3 6-3" />
              <path d="M2 12c5 0 4-6 9-6" opacity=".45" />
            </svg>
          </button>
        </div>
      </section>

      <ControlRack
        band={band}
        defaultBand={defaults}
        selected={selected}
        outputDb={params.outputDb}
        mix={params.mix}
        onBandChange={(patch) => updateBand(selected, patch)}
        onGlobalChange={updateGlobal}
      />
    </main>
  )
}

export default App
