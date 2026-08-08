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
  /// The rate the host is actually running at, threaded into the curve maths.
  /// Drawing at a fixed 48 kHz made the picture disagree with what you hear on
  /// any other rate, most visibly at the top of the range where the bilinear
  /// transform warps hardest.
  const [sampleRate, setRate] = useState(DEFAULT_SAMPLE_RATE)

  /// Analyser frames land in a ref, never in state: at ~30 Hz a `setState` per
  /// frame would rerender every curve, node and label in the editor to repaint
  /// one canvas that reads the value itself.
  const spectrum = useRef<SpectrumFrame | null>(null)

  useEffect(
    () =>
      connectBridge(
        (nativeParams) => {
          setParams(nativeParams)
          setPreset(matchingPresetIndex(nativeParams))
        },
        (isConnected) => {
          setConnected(isConnected)
          // Nothing is measuring any more — drop the last frame so the overlay
          // does not sit frozen on a picture of audio that stopped.
          if (!isConnected) spectrum.current = null
        },
        (frame) => {
          spectrum.current = frame
        },
        (rate) => setRate(rate),
      ),
    [],
  )

  /// Apply a band edit, switching the band on if it was off.
  ///
  /// Every band opens inactive and flat (see `DEFAULT_PARAMS`), so the first
  /// thing anyone does is reach for one. Dragging its node or turning one of
  /// its knobs *is* the request to use it: shaping a band that stays silent
  /// reads as a broken control, and the alternative is hunting for the chip
  /// or the toggle before every first move. An explicit `active` in the patch
  /// — the chip's double-click, the rack's switch — is left exactly as asked,
  /// so turning a band off still turns it off.
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

  /// Audition one band's frequency region on its own, FabFilter-style.
  ///
  /// Soloing is exclusive and self-clearing: clicking the lit band turns it
  /// off, and clicking another moves the audition rather than stacking. It is
  /// deliberately kept out of `updateGlobal`/preset state — see `presets.ts`.
  const toggleSolo = useCallback((index: number) => {
    setParams((current) => {
      const next = current.soloBand === index ? SOLO_NONE : index
      postParam('soloBand', next)
      return { ...current, soloBand: next }
    })
  }, [])

  /// Set solo outright rather than toggling.
  ///
  /// The momentary right-drag audition needs this: it has to force solo on at
  /// gesture start and put back whatever was there on release, neither of which
  /// a toggle can express. No-ops when already on the requested band so a drag
  /// does not post a parameter per frame.
  const setSolo = useCallback((index: number) => {
    setParams((current) => {
      if (current.soloBand === index) return current
      postParam('soloBand', index)
      return { ...current, soloBand: index }
    })
  }, [])

  /// Solo is an audition, not a setting: leaving the EQ silently stuck
  /// bandpassing one band would be a nasty surprise, so it drops on Escape and
  /// on unmount (closing the editor window) alike. The ref exists purely so the
  /// unmount cleanup can see the latest value without resubscribing per change.
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
      // The component is going away, so only the DSP-side release matters here.
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
          sampleRate={sampleRate}
          bands={params.bands}
          selected={selected}
          bypassed={!params.power}
          showBandCurves={showBandCurves}
          showSpectrum={showSpectrum}
          spectrumRef={spectrum}
          soloBand={params.soloBand}
          onSelect={setSelected}
          onBandChange={updateBand}
          onToggleSolo={toggleSolo}
          onSetSolo={setSolo}
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
