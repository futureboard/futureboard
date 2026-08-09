import { useCallback, useEffect, useRef, useState } from 'react'
import { AnimatePresence, motion } from 'motion/react'
import { animate } from 'animejs'
import {
  ArrowCounterClockwiseIcon,
  CaretDownIcon,
  CaretLeftIcon,
  CaretRightIcon,
  ChartBarIcon,
  WaveSineIcon,
} from '@phosphor-icons/react'
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
import {
  DEFAULT_PARAMS,
  FACTORY_PRESETS,
  cloneParams,
  matchingPresetIndex,
  postAllParams,
  postBandPatch,
} from './lib/presets'
import { BypassSwitch, IconButton } from './components/Controls'
import { BandChips } from './components/BandChips'
import { ControlRack } from './components/ControlRack'
import { PresetMenu } from './components/PresetMenu'
import { ResponseGraph } from './components/ResponseGraph'

function App() {
  const [params, setParams] = useState<EqParams>(DEFAULT_PARAMS)
  const [selected, setSelected] = useState(0)
  const [connected, setConnected] = useState(false)
  const [showBandCurves, setShowBandCurves] = useState(true)
  const [showSpectrum, setShowSpectrum] = useState(true)
  const [preset, setPreset] = useState<number | null>(0)
  const [presetOpen, setPresetOpen] = useState(false)
  const [sampleRate, setRate] = useState(DEFAULT_SAMPLE_RATE)
  const spectrum = useRef<SpectrumFrame | null>(null)
  const rootRef = useRef<HTMLElement>(null)
  const rackRef = useRef<HTMLElement>(null)
  const stageRef = useRef<HTMLElement>(null)
  const presetAnchor = useRef<HTMLButtonElement | null>(null)
  const presetNameRef = useRef<HTMLSpanElement | null>(null)
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
          setPresetOpen(false)
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
      if (event.key === 'Escape' && !presetOpen) clearSolo()
    }
    window.addEventListener('keydown', onKeyDown)
    return () => {
      window.removeEventListener('keydown', onKeyDown)
      if (soloRef.current !== SOLO_NONE) postParam('soloBand', SOLO_NONE)
    }
  }, [clearSolo, presetOpen])

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

  const stepPreset = (delta: -1 | 1) => {
    loadPreset((preset ?? (delta > 0 ? -1 : 0)) + delta)
    if (presetNameRef.current) {
      animate(presetNameRef.current, {
        x: [
          { to: delta * 12, duration: 0 },
          { to: 0, duration: 300 },
        ],
        opacity: [
          { to: 0, duration: 0 },
          { to: 1, duration: 240 },
        ],
        ease: 'outExpo',
      })
    }
  }

  const band = params.bands[selected]
  if (!band) return null
  const defaults = DEFAULT_PARAMS.bands[selected]
  if (!defaults) return null

  const presetLabel =
    preset === null ? 'Modified' : (FACTORY_PRESETS[preset]?.name ?? 'Modified')

  return (
    <main
      ref={rootRef}
      className={`editor flex h-full w-full flex-col overflow-hidden bg-workspace${
        params.power ? '' : ' is-bypassed'
      }`}
    >
      <header className="flex h-12 shrink-0 items-center gap-4 border-b border-hairline bg-panel px-4">
        <div className="flex min-w-0 items-center gap-2.5">
          <div className="min-w-0">
            <h1 className="text-[15px] font-bold tracking-[-0.01em]">EQUZ8</h1>
            <p className="label-cap tracking-[0.16em]" style={{ fontSize: 8 }}>
              Dynamic EQ
            </p>
          </div>
          <span
            className="flex items-center gap-1.5 rounded border border-hairline-hi px-2 py-0.5 text-[10px] font-medium text-ink-muted"
            title={
              connected
                ? 'Linked to the DSP instance'
                : 'Preview — no DSP instance bound'
            }
          >
            <motion.span
              aria-hidden
              className="h-1.5 w-1.5 rounded-full"
              style={{
                background: connected ? 'var(--color-signal)' : 'var(--color-hairline-hi)',
              }}
              animate={connected ? { opacity: [0.45, 1, 0.45] } : { opacity: 1 }}
              transition={
                connected
                  ? { duration: 1.8, repeat: Infinity, ease: 'easeInOut' }
                  : { duration: 0.2 }
              }
            />
            {connected ? 'Linked' : 'Standby'}
          </span>
        </div>

        <div className="flex flex-1 justify-center">
          <div className="flex items-center gap-1">
            <IconButton label="Previous preset" onClick={() => stepPreset(-1)}>
              <CaretLeftIcon size={13} weight="bold" />
            </IconButton>
            <button
              ref={presetAnchor}
              type="button"
              aria-haspopup="dialog"
              aria-expanded={presetOpen}
              onClick={() => setPresetOpen((open) => !open)}
              className="flex h-8 w-64 cursor-pointer items-center gap-2 rounded border border-hairline bg-well px-2.5 transition-colors duration-150 hover:border-hairline-hi"
            >
              <span className="w-3 shrink-0" />
              <span
                ref={presetNameRef}
                className="min-w-0 flex-1 truncate text-center text-[12px] font-medium"
              >
                {presetLabel}
              </span>
              <motion.span
                animate={{ rotate: presetOpen ? 180 : 0 }}
                transition={{ duration: 0.16, ease: 'easeOut' }}
                className="grid shrink-0 place-items-center text-ink-dim"
              >
                <CaretDownIcon size={12} weight="bold" />
              </motion.span>
            </button>
            <AnimatePresence>
              {presetOpen && (
                <PresetMenu
                  anchorRef={presetAnchor}
                  currentIndex={preset}
                  onLoad={loadPreset}
                  onClose={() => setPresetOpen(false)}
                />
              )}
            </AnimatePresence>
            <IconButton label="Next preset" onClick={() => stepPreset(1)}>
              <CaretRightIcon size={13} weight="bold" />
            </IconButton>
          </div>
        </div>

        <div className="flex items-center gap-2">
          <IconButton label="Load default preset" onClick={() => loadPreset(0)}>
            <ArrowCounterClockwiseIcon size={14} weight="bold" />
          </IconButton>
          <BypassSwitch
            on={params.power}
            label={params.power ? 'Equalizer enabled' : 'Equalizer bypassed'}
            onToggle={() => updateGlobal({ power: !params.power })}
          />
        </div>
      </header>

      <section
        ref={stageRef}
        className="stage relative mx-3 mt-2 min-h-0 flex-1 overflow-hidden rounded-t-md border border-b-0 border-hairline bg-graph-floor shadow-[inset_0_1px_18px_rgb(0_0_0_/_0.45)]"
      >
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

        <div className="pointer-events-none absolute inset-x-3 top-2 z-[2] flex items-start justify-between">
          <div className="pointer-events-auto">
            <BandChips
              bands={params.bands}
              selected={selected}
              onSelect={selectBand}
              onToggle={(index) =>
                updateBand(index, { active: !params.bands[index]?.active })
              }
            />
          </div>
          <div className="pointer-events-auto flex gap-0.5 rounded border border-hairline bg-well/70 p-0.5 backdrop-blur-sm">
            <IconButton
              label="Show the input spectrum"
              active={showSpectrum}
              onClick={() => setShowSpectrum((value) => !value)}
            >
              <ChartBarIcon size={13} weight={showSpectrum ? 'fill' : 'bold'} />
            </IconButton>
            <IconButton
              label="Show each band's curve"
              active={showBandCurves}
              onClick={() => setShowBandCurves((value) => !value)}
            >
              <WaveSineIcon size={13} weight={showBandCurves ? 'fill' : 'bold'} />
            </IconButton>
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
