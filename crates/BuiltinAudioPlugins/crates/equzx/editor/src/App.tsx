import { useCallback, useEffect, useRef, useState } from 'react'
import { animate, stagger } from 'animejs'
import { AudioEngine } from './audio/AudioEngine'
import { EQDisplay, type AnalyzerMode } from './components/EQDisplay'
import { AnalyzerOverlay } from './components/AnalyzerOverlay'
import { BandStrip } from './components/BandStrip'
import { Header } from './components/Header'
import { Transport } from './components/Transport'
import { PanelResizer } from './components/PanelResizer'
import { MAX_BANDS, defaultBands, makeBand, type Band, type ChannelView } from './dsp/bands'
import { cloneSnapshot, emptySnapshot, type Snapshot } from './state/presets'

/**
 * One engine for the page lifetime. Module scope rather than state so it survives
 * StrictMode's double-mount without opening a second AudioContext.
 */
const PANEL_DEFAULT = 232
const PANEL_MIN = 176
const PANEL_KEY = 'equzfree.panelHeight'

function readPanelHeight(): number {
  try {
    const v = Number(localStorage.getItem(PANEL_KEY))
    return Number.isFinite(v) && v >= PANEL_MIN ? v : PANEL_DEFAULT
  } catch {
    return PANEL_DEFAULT
  }
}

let sharedEngine: AudioEngine | null = null
function getEngine(): AudioEngine {
  sharedEngine ??= new AudioEngine()
  return sharedEngine
}

export default function App() {
  const [bands, setBands] = useState<Band[]>(defaultBands)
  const [selectedId, setSelectedId] = useState<number | null>(null)
  const [soloId, setSoloId] = useState<number | null>(null)
  const [bypassed, setBypassed] = useState(false)
  const [dbRange, setDbRange] = useState(18)
  const [analyzerMode, setAnalyzerMode] = useState<AnalyzerMode>('both')
  const [spectrumSmoothing, setSpectrumSmoothing] = useState(1 / 12)
  const [channelView, setChannelView] = useState<ChannelView>('all')
  const [outputGain, setOutputGain] = useState(0)

  const engine = getEngine()
  const [fileName, setFileName] = useState('')
  const [loading, setLoading] = useState(false)
  const [playing, setPlaying] = useState(false)
  const [loop, setLoop] = useState(true)
  const [position, setPosition] = useState(0)
  const [duration, setDuration] = useState(0)
  const [dragOver, setDragOver] = useState(false)
  const [panelHeight, setPanelHeight] = useState(readPanelHeight)
  const [viewportH, setViewportH] = useState(() => window.innerHeight)

  // A/B: the live state is whichever slot is active; the other one is parked here.
  const [slot, setSlot] = useState<'A' | 'B'>('A')
  const [parked, setParked] = useState<Snapshot>(emptySnapshot)

  const shellRef = useRef<HTMLDivElement>(null)
  const dropRef = useRef<HTMLDivElement>(null)
  const dragDepth = useRef(0)

  // Both bars float over the display, so the plot has to be told how much room to
  // leave for them — the header wraps and the bottom panel is user-resizable, so
  // neither height is a constant.
  const headerRef = useRef<HTMLDivElement>(null)
  const bottomRef = useRef<HTMLDivElement>(null)
  const [headerH, setHeaderH] = useState(70)
  const [bottomH, setBottomH] = useState(280)
  useEffect(() => {
    const observe = (el: HTMLElement | null, set: (h: number) => void) => {
      if (!el) return () => {}
      const ro = new ResizeObserver(([entry]) => set(entry.contentRect.height))
      ro.observe(el)
      return () => ro.disconnect()
    }
    const stopTop = observe(headerRef.current, setHeaderH)
    const stopBottom = observe(bottomRef.current, setBottomH)
    return () => {
      stopTop()
      stopBottom()
    }
  }, [])

  // --- engine sync -------------------------------------------------------
  useEffect(() => engine.setBands(bands), [engine, bands])
  useEffect(() => engine.setSolo(soloId), [engine, soloId])
  useEffect(() => engine.setBypass(bypassed), [engine, bypassed])
  useEffect(() => engine.setOutputGain(outputGain), [engine, outputGain])
  useEffect(() => engine.setLoop(loop), [engine, loop])

  // Poll the playhead; the engine owns the clock.
  useEffect(() => {
    let raf = 0
    const tick = () => {
      raf = requestAnimationFrame(tick)
      setPosition(engine.position)
      if (engine.isPlaying !== playing) setPlaying(engine.isPlaying)
    }
    raf = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(raf)
  }, [engine, playing])

  // --- band editing ------------------------------------------------------
  const patch = useCallback((id: number, p: Partial<Band>) => {
    setBands((prev) => prev.map((b) => (b.id === id ? { ...b, ...p } : b)))
  }, [])

  // A band created while looking at one channel belongs to that channel — otherwise
  // it would appear in a view that isn't showing it.
  const addBand = useCallback(
    (freq = 1000, gain = 0) => {
      setBands((prev) => {
        if (prev.length >= MAX_BANDS) return prev
        const band = makeBand({
          freq,
          gain,
          q: 1,
          channel: channelView === 'all' ? 'stereo' : channelView,
        })
        setSelectedId(band.id)
        return [...prev, band].sort((a, b) => a.freq - b.freq)
      })
    },
    [channelView],
  )

  const removeBand = useCallback((id: number) => {
    setBands((prev) => prev.filter((b) => b.id !== id))
    setSelectedId((cur) => (cur === id ? null : cur))
    setSoloId((cur) => (cur === id ? null : cur))
  }, [])

  const reset = useCallback(() => {
    setBands(defaultBands())
    setSelectedId(null)
    setSoloId(null)
    setBypassed(false)
    setOutputGain(0)
  }, [])

  // The panel may never squeeze the EQ display out of existence. Both floating
  // bars and their margins come out of the same budget, hence the deep reserve.
  const panelMax = Math.max(PANEL_MIN, viewportH - 380)
  const panelH = Math.min(panelHeight, panelMax)

  useEffect(() => {
    const onResize = () => setViewportH(window.innerHeight)
    window.addEventListener('resize', onResize)
    return () => window.removeEventListener('resize', onResize)
  }, [])

  const changePanelHeight = useCallback((h: number) => {
    setPanelHeight(h)
    try {
      localStorage.setItem(PANEL_KEY, String(h))
    } catch {
      /* storage blocked — the height just won't persist */
    }
  }, [])

  // --- A/B compare & presets ---------------------------------------------
  const snapshot = useCallback((): Snapshot => ({ bands, outputGain }), [bands, outputGain])

  const applySnapshot = useCallback((snap: Snapshot) => {
    setBands(snap.bands)
    setOutputGain(snap.outputGain)
    // Ids differ between slots and presets, so nothing stays selected across a swap.
    setSelectedId(null)
    setSoloId(null)
  }, [])

  const switchSlot = useCallback(() => {
    const live = snapshot()
    applySnapshot(parked)
    setParked(live)
    setSlot((s) => (s === 'A' ? 'B' : 'A'))
  }, [snapshot, parked, applySnapshot])

  const copyToOther = useCallback(() => {
    setParked(cloneSnapshot(snapshot()))
  }, [snapshot])

  // --- file loading ------------------------------------------------------
  const loadFile = useCallback(
    async (file: File) => {
      setLoading(true)
      try {
        await engine.loadFile(file)
        setFileName(file.name)
        setDuration(engine.duration)
        await engine.play()
        setPlaying(true)
      } catch (err) {
        console.error(err)
        setFileName(`Could not decode "${file.name}"`)
      } finally {
        setLoading(false)
      }
    },
    [engine],
  )

  // --- drag & drop -------------------------------------------------------
  useEffect(() => {
    const onEnter = (ev: DragEvent) => {
      ev.preventDefault()
      dragDepth.current++
      if (ev.dataTransfer?.types.includes('Files')) setDragOver(true)
    }
    const onOver = (ev: DragEvent) => ev.preventDefault()
    const onLeave = (ev: DragEvent) => {
      ev.preventDefault()
      dragDepth.current = Math.max(0, dragDepth.current - 1)
      if (dragDepth.current === 0) setDragOver(false)
    }
    const onDrop = (ev: DragEvent) => {
      ev.preventDefault()
      dragDepth.current = 0
      setDragOver(false)
      const file = ev.dataTransfer?.files?.[0]
      if (file) void loadFile(file)
    }
    window.addEventListener('dragenter', onEnter)
    window.addEventListener('dragover', onOver)
    window.addEventListener('dragleave', onLeave)
    window.addEventListener('drop', onDrop)
    return () => {
      window.removeEventListener('dragenter', onEnter)
      window.removeEventListener('dragover', onOver)
      window.removeEventListener('dragleave', onLeave)
      window.removeEventListener('drop', onDrop)
    }
  }, [loadFile])

  useEffect(() => {
    if (!dropRef.current) return
    animate(dropRef.current, {
      opacity: dragOver ? [0, 1] : [1, 0],
      scale: dragOver ? [1.03, 1] : [1, 1.02],
      duration: 220,
      ease: 'outQuad',
    })
  }, [dragOver])

  // --- intro animation ---------------------------------------------------
  useEffect(() => {
    const nodes = shellRef.current?.querySelectorAll('[data-intro]')
    if (!nodes?.length) return
    animate(nodes, {
      opacity: [0, 1],
      translateY: [12, 0],
      duration: 620,
      delay: stagger(70),
      ease: 'outCubic',
    })
  }, [])

  const togglePlay = useCallback(() => {
    if (!engine.hasAudio) return
    if (engine.isPlaying) {
      engine.pause()
      setPlaying(false)
    } else {
      void engine.play().then(() => setPlaying(true))
    }
  }, [engine])

  // --- keyboard ----------------------------------------------------------
  useEffect(() => {
    const onKey = (ev: KeyboardEvent) => {
      const target = ev.target as HTMLElement
      const tag = target.tagName
      // Don't hijack keys while a preset name / slider / dropdown has focus.
      if (tag === 'INPUT' || tag === 'SELECT' || tag === 'TEXTAREA' || target.isContentEditable) return
      if (ev.code === 'Space') {
        ev.preventDefault()
        togglePlay()
      } else if (ev.key === 'Delete' || ev.key === 'Backspace') {
        if (selectedId !== null) removeBand(selectedId)
      } else if (ev.key.toLowerCase() === 'b') {
        setBypassed((v) => !v)
      } else if (ev.key === 'Escape') {
        setSelectedId(null)
      } else if (ev.key.toLowerCase() === 'x') {
        switchSlot()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  })

  return (
    <div className="h-screen w-screen overflow-hidden bg-[#070708] text-white/90">
      <div ref={shellRef} className="relative flex h-full w-full flex-col bg-[#0b0b0d]">
        <div ref={headerRef} data-intro className="absolute inset-x-3 top-3 z-30">
          <Header
            channelView={channelView}
            dbRange={dbRange}
            outputGain={outputGain}
            bypassed={bypassed}
            slot={slot}
            otherSlotFilled={parked.bands.length > 0}
            getSnapshot={snapshot}
            onLoadSnapshot={applySnapshot}
            onSwitchSlot={switchSlot}
            onCopyToOther={copyToOther}
            onChannelView={setChannelView}
            onDbRange={setDbRange}
            onOutputGain={setOutputGain}
            onBypass={setBypassed}
            onReset={reset}
          />
        </div>

        <div
          data-intro
          className="relative min-h-0 flex-1 bg-gradient-to-b from-[#0a0a0b] to-[#121214]"
          // 12px to clear each bar's offset from the window edge, plus a gap so
          // the plot never runs right up under a floating edge.
          style={{ paddingTop: headerH + 20, paddingBottom: bottomH + 20 }}
        >
          {/* Ambient light for the bar to pick up — glass over a flat field reads as plastic. */}
          <div
            className="pointer-events-none absolute inset-x-0 top-0 h-48 bg-[radial-gradient(120%_100%_at_18%_0%,rgba(255,77,157,0.18),transparent_60%),radial-gradient(90%_100%_at_88%_0%,rgba(255,211,228,0.10),transparent_62%)]"
            aria-hidden
          />
          <div className="relative h-full w-full">
            <EQDisplay
              bands={bands}
              selectedId={selectedId}
              soloId={soloId}
              bypassed={bypassed}
              dbRange={dbRange}
              analyzerMode={analyzerMode}
              spectrumSmoothing={spectrumSmoothing}
              channelView={channelView}
              engine={engine}
              canAdd={bands.length < MAX_BANDS}
              onPatch={patch}
              onSelect={setSelectedId}
              onSolo={setSoloId}
              onAdd={(f, g) => addBand(f, g)}
              onRemove={removeBand}
            />
            {/* Sits inside the plot, top-right, over the analyser it controls. */}
            <div className="absolute right-3 top-3 z-20">
              <AnalyzerOverlay
                analyzerMode={analyzerMode}
                spectrumSmoothing={spectrumSmoothing}
                onAnalyzerMode={setAnalyzerMode}
                onSpectrumSmoothing={setSpectrumSmoothing}
              />
            </div>
          </div>
          {!engine.hasAudio && !loading && (
            <div className="pointer-events-none absolute inset-x-0 top-[62%] text-center text-[11px] text-white/25">
              Drop an audio file anywhere to hear the EQ · click the display to add a band ·
              scroll a handle for Q · right-drag a handle to solo · X swaps A/B
            </div>
          )}
        </div>

        {/* Transport, resizer and band panel travel together as one floating slab. */}
        <div ref={bottomRef} data-intro className="absolute inset-x-3 bottom-3 z-30">
          <div className="glass overflow-hidden rounded-[22px]">
            <Transport
              fileName={fileName}
              hasAudio={engine.hasAudio}
              loading={loading}
              playing={playing}
              loop={loop}
              position={position}
              duration={duration}
              onPlayPause={togglePlay}
              onStop={() => {
                engine.stop()
                setPlaying(false)
              }}
              onLoop={setLoop}
              onSeek={(t) => engine.seek(t)}
              onFile={loadFile}
            />

            <PanelResizer
              height={panelH}
              min={PANEL_MIN}
              max={panelMax}
              defaultHeight={PANEL_DEFAULT}
              onChange={changePanelHeight}
            />

            <BandStrip
              bands={bands}
              selectedId={selectedId}
              soloId={soloId}
              engine={engine}
              onSelect={setSelectedId}
              onPatch={patch}
              onRemove={removeBand}
              onSolo={setSoloId}
              height={panelH}
            />
          </div>
        </div>
      </div>

      <div
        ref={dropRef}
        className={`fixed inset-0 z-50 grid place-items-center bg-black/70 backdrop-blur-sm ${
          dragOver ? '' : 'pointer-events-none opacity-0'
        }`}
      >
        <div className="rounded-[28px] border-2 border-dashed border-neon/70 px-16 py-12 text-center shadow-[0_0_60px_-10px_rgba(255,77,157,0.5)]">
          <div className="text-2xl font-semibold text-white">Drop audio to preview</div>
          <div className="mt-1 text-[12px] text-white/45">WAV · MP3 · FLAC · OGG · M4A</div>
        </div>
      </div>
    </div>
  )
}
