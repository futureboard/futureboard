import { useCallback, useEffect, useRef, useState } from 'react'
import {
  SOLO_NONE,
  connectBridge,
  postParam,
  type Band,
  type EqParams,
  type SpectrumFrame,
} from './bridge'
import { DEFAULT_SAMPLE_RATE } from './lib/eq'
import {
  flashControlRack,
  playEditorIntro,
  tweenBypass,
} from './lib/motion'
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
  const [selected, setSelected] = useState(0)
  const [connected, setConnected] = useState(false)
  const [showBandCurves, setShowBandCurves] = useState(true)
  const [showSpectrum, setShowSpectrum] = useState(true)
  const [preset, setPreset] = useState<number | null>(0)
  const [sampleRate, setRate] = useState(DEFAULT_SAMPLE_RATE)
  const spectrum = useRef<SpectrumFrame | null>(null)
  const rootRef = useRef<HTMLElement>(null)
  const rackRef = useRef<HTMLElement>(null)
  const stageRef = useRef<HTMLElement>(null)
  const introPlayed = useRef(false)

  useEffect(() => {
    if (introPlayed.current) return
    introPlayed.current = true
    playEditorIntro(rootRef.current)
  }, [])

  useEffect(() => {
    tweenBypass(stageRef.current, !params.power)
  }, [params.power])

  useEffect(
    () =>
      connectBridge(
        (nativeParams) => {
          setParams(nativeParams)
          setPreset(matchingPresetIndex(nativeParams))
        },
        (isConnected) => {
          setConnected(isConnected)
          if (!isConnected) spectrum.current = null
        },
        (frame) => {
          spectrum.current = frame
        },
        (rate) => setRate(rate),
      ),
    [],
  )

  const updateBand = useCallback((index: number, patch: Partial<Band>) => {
    setParams((current) => {
      const band = current.bands[index]
      if (!band) return current
      const effective =
        patch.active === undefined && !band.active && Object.keys(patch).length > 0
          ? { ...patch, active: true }
          : patch
      postBandPatch(index, effective)
      return {
        ...current,
        bands: current.bands.map((entry, bandIndex) =>
          bandIndex === index ? { ...entry, ...effective } : entry,
        ),
      }
    })
    setPreset(null)
  }, [])

  const toggleSolo = useCallback((index: number) => {
    setParams((current) => {
      const next = current.soloBand === index ? SOLO_NONE : index
      postParam('soloBand', next)
      return { ...current, soloBand: next }
    })
  }, [])

  const setSolo = useCallback((index: number) => {
    setParams((current) => {
      if (current.soloBand === index) return current
      postParam('soloBand', index)
      return { ...current, soloBand: index }
    })
  }, [])

  const soloRef = useRef(params.soloBand)
  useEffect(() => {
    soloRef.current = params.soloBand
  }, [params.soloBand])

  const clearSolo = useCallback(() => {
    setParams((current) => {
      if (current.soloBand === SOLO_NONE) return current
      postParam('soloBand', SOLO_NONE)
      return { ...current, soloBand: SOLO_NONE }
    })
  }, [])

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') clearSolo()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => {
      window.removeEventListener('keydown', onKeyDown)
      if (soloRef.current !== SOLO_NONE) postParam('soloBand', SOLO_NONE)
    }
  }, [clearSolo])

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

  const selectBand = useCallback((index: number) => {
    setSelected((current) => {
      if (current !== index) flashControlRack(rackRef.current)
      return index
    })
  }, [])

  const loadPreset = useCallback((index: number) => {
    const wrapped = (index + FACTORY_PRESETS.length) % FACTORY_PRESETS.length
    const entry = FACTORY_PRESETS[wrapped]
    if (!entry) return
    const next = cloneParams(entry.params)
    setParams(next)
    setPreset(wrapped)
    postAllParams(next)
    flashControlRack(rackRef.current)
  }, [])

  const band = params.bands[selected]
  if (!band) return null

  const defaults = DEFAULT_PARAMS.bands[selected]
  if (!defaults) return null

  return (
    <main
      ref={rootRef}
      className={`editor${params.power ? '' : ' is-bypassed'}`}
    >
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

      <section ref={stageRef} className="stage">
        <ResponseGraph
          sampleRate={sampleRate}
          bands={params.bands}
          selected={selected}
          bypassed={!params.power}
          showBandCurves={showBandCurves}
          showSpectrum={showSpectrum}
          spectrumRef={spectrum}
          soloBand={params.soloBand}
          onSelect={selectBand}
          onBandChange={updateBand}
          onToggleSolo={toggleSolo}
          onSetSolo={setSolo}
        />

        <div className="stage-overlay">
          <BandChips
            bands={params.bands}
            selected={selected}
            onSelect={selectBand}
            onToggle={(index) =>
              updateBand(index, { active: !params.bands[index]?.active })
            }
          />
          <div className="graph-options">
            <button
              type="button"
              className={`graph-option${showSpectrum ? ' is-on' : ''}`}
              aria-pressed={showSpectrum}
              title="Show the input spectrum"
              aria-label="Show the input spectrum"
              onClick={() => setShowSpectrum((value) => !value)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M3 20v-5M7 20v-9M11 20V6M15 20v-8M19 20v-4" />
              </svg>
            </button>
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
        </div>
      </section>

      <ControlRack
        rackRef={rackRef}
        band={band}
        defaultBand={defaults}
        selected={selected}
        outputDb={params.outputDb}
        mix={params.mix}
        soloed={params.soloBand === selected}
        onBandChange={(patch) => updateBand(selected, patch)}
        onGlobalChange={updateGlobal}
        onToggleSolo={() => toggleSolo(selected)}
      />
    </main>
  )
}

export default App
