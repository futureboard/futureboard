import { butterworthQs } from '../dsp/biquad'
import { IS_CUT, USES_GAIN, type Band } from '../dsp/bands'
import { dynamicStep, rmsDb } from '../dsp/dynamics'

export type EngineState = 'empty' | 'loading' | 'ready' | 'playing'

interface Listeners {
  onStateChange?: (state: EngineState) => void
  onEnded?: () => void
}

/** Sidechain path that measures how much energy sits in one band's region. */
interface Detector {
  filter: BiquadFilterNode
  analyser: AnalyserNode
  buf: Float32Array<ArrayBuffer>
  /** Smoothed engagement, 0..1. */
  env: number
  /** Last measured band level in dBFS, for the meter. */
  level: number
  /** Node currently feeding the filter — the full input, or one M/S bus. */
  source: AudioNode | null
}

/**
 * The Web Audio spec fixes AnalyserNode's fftSize range at [32, 32768], so that is
 * the hard ceiling — past it you need a custom FFT over an AudioWorklet's raw
 * samples rather than an AnalyserNode.
 */
const MAX_FFT_SIZE = 32768

/** Visualiser FFT size. Larger = finer frequency detail but a longer, laggier window. */
export const FFT_SIZE = Math.min(16384, MAX_FFT_SIZE)

/** Bins the analyser hands back per frame — always half the FFT size. */
export const ANALYSER_BINS = FFT_SIZE / 2

/**
 * Web Audio graph. With every band set to stereo it is one serial chain:
 *
 *   source -> inputGain -+-> preAnalyser
 *                        +-> [detector filter -> analyser] per dynamic band
 *                        |
 *                        +-> [band biquads...] -> [solo] -> outputGain -> postAnalyser -> out
 *
 * As soon as one band is mid- or side-only the chain splits into an M/S pair,
 * and stereo bands are instantiated twice — once per side — which is identical
 * to filtering L/R, since M/S is a linear transform and the two filters match:
 *
 *   inputGain -> splitter -> [0.5L+0.5R] -> midIn  -> [mid + stereo bands] -+-> merger -> ...
 *                         -> [0.5L-0.5R] -> sideIn -> [side + stereo bands] +
 *
 * The band nodes mirror `dsp/bands.ts` exactly (same Butterworth cascades), so the
 * drawn curve and the audible result are the same filter.
 */
export class AudioEngine {
  readonly ctx: AudioContext
  readonly preAnalyser: AnalyserNode
  readonly postAnalyser: AnalyserNode

  private inputGain: GainNode
  private outputGain: GainNode
  /**
   * Chain nodes keyed by band id. A band owns one cascade per path it sits in —
   * one for a serial or single-channel band, two for a stereo band in M/S mode.
   */
  private bandNodes = new Map<number, BiquadFilterNode[][]>()
  private soloNodes: BiquadFilterNode[] = []
  private detectors = new Map<number, Detector>()

  // --- M/S encode / decode, built once and rewired per rebuild ---
  /** Forces a stereo pair ahead of the splitter, so mono material still lands in both. */
  private stereoIn: GainNode
  private splitter: ChannelSplitterNode
  private merger: ChannelMergerNode
  /** Encode matrix: L and R contributions to mid and to side. */
  private encMidL: GainNode
  private encMidR: GainNode
  private encSideL: GainNode
  private encSideR: GainNode
  /** Heads of the two processing paths. */
  private midIn: GainNode
  private sideIn: GainNode
  /** Decode matrix: L = mid + side, R = mid - side. */
  private midToL: GainNode
  private midToR: GainNode
  private sideToL: GainNode
  private sideToR: GainNode

  private buffer: AudioBuffer | null = null
  private source: AudioBufferSourceNode | null = null
  private startedAt = 0
  private pausedAt = 0
  private playing = false

  private bands: Band[] = []
  private soloId: number | null = null
  private bypassed = false
  private topology = ''

  /** Current dynamic gain offset per band id, in dB. Read by the display. */
  private deltas = new Map<number, number>()
  private pumpHandle = 0
  private lastPump = 0

  private listeners: Listeners
  loop = true
  fileName = ''

  constructor(listeners: Listeners = {}) {
    this.listeners = listeners
    this.ctx = new AudioContext()
    this.inputGain = this.ctx.createGain()
    this.outputGain = this.ctx.createGain()

    this.splitter = this.ctx.createChannelSplitter(2)
    this.merger = this.ctx.createChannelMerger(2)
    const gain = (v: number) => {
      const g = this.ctx.createGain()
      g.gain.value = v
      return g
    }

    // A splitter up-mixes with 'discrete' interpretation, which pads a mono input
    // with a silent second channel — that would put the whole file in L and leave
    // side equal to mid. Forcing a speakers-mode stereo pair first copies mono to
    // both, so mid carries the material and side comes out silent, as it should.
    this.stereoIn = gain(1)
    this.stereoIn.channelCount = 2
    this.stereoIn.channelCountMode = 'explicit'
    this.stereoIn.channelInterpretation = 'speakers'
    this.encMidL = gain(0.5)
    this.encMidR = gain(0.5)
    this.encSideL = gain(0.5)
    this.encSideR = gain(-0.5)
    this.midIn = gain(1)
    this.sideIn = gain(1)
    this.midToL = gain(1)
    this.midToR = gain(1)
    this.sideToL = gain(1)
    this.sideToR = gain(-1)

    this.preAnalyser = this.ctx.createAnalyser()
    this.postAnalyser = this.ctx.createAnalyser()
    for (const a of [this.preAnalyser, this.postAnalyser]) {
      a.fftSize = FFT_SIZE
      // Inter-frame averaging: ~75 ms at 60 fps, against a ~340 ms analysis window.
      a.smoothingTimeConstant = 0.7
      a.minDecibels = -110
      a.maxDecibels = -5
    }

    this.inputGain.connect(this.preAnalyser)
    this.outputGain.connect(this.postAnalyser)
    this.outputGain.connect(this.ctx.destination)
    this.rebuild()

    this.lastPump = performance.now()
    this.pump = this.pump.bind(this)
    this.pumpHandle = requestAnimationFrame(this.pump)
  }

  get sampleRate() {
    return this.ctx.sampleRate
  }
  get duration() {
    return this.buffer?.duration ?? 0
  }
  get isPlaying() {
    return this.playing
  }
  get hasAudio() {
    return this.buffer !== null
  }

  /** Current playhead in seconds. */
  get position(): number {
    if (!this.buffer) return 0
    if (!this.playing) return this.pausedAt
    const raw = this.pausedAt + (this.ctx.currentTime - this.startedAt)
    return this.loop ? raw % this.buffer.duration : Math.min(raw, this.buffer.duration)
  }

  /** Dynamic gain offset for a band, in dB (0 when not dynamic or not engaged). */
  getDelta(id: number): number {
    return this.deltas.get(id) ?? 0
  }

  /** Measured band level in dBFS, for the threshold meter. */
  getLevel(id: number): number {
    return this.detectors.get(id)?.level ?? -100
  }

  // --- graph -------------------------------------------------------------

  /** Identity of the node topology; a change here means the graph must be rebuilt. */
  private signature(): string {
    const solo = this.soloId ?? 'none'
    const chain = this.bypassed
      ? 'bypass'
      : this.bands
          .map((b) => `${b.id}:${b.type}:${b.channel}:${b.enabled ? 1 : 0}:${b.slope}`)
          .join('|')
    return `${chain}#${solo}`
  }

  /** The bands that end up in the graph, in order. */
  private activeBands(): Band[] {
    if (this.bypassed) return []
    if (this.soloId !== null) return this.bands.filter((b) => b.id === this.soloId)
    return this.bands.filter((b) => b.enabled)
  }

  private rebuild() {
    for (const groups of this.bandNodes.values()) {
      for (const nodes of groups) for (const n of nodes) n.disconnect()
    }
    for (const n of this.soloNodes) n.disconnect()
    this.inputGain.disconnect()
    for (const n of [
      this.stereoIn, this.splitter, this.merger,
      this.encMidL, this.encMidR, this.encSideL, this.encSideR,
      this.midIn, this.sideIn, this.midToL, this.midToR, this.sideToL, this.sideToR,
    ]) {
      n.disconnect()
    }
    this.inputGain.connect(this.preAnalyser)

    this.bandNodes = new Map()
    this.soloNodes = []

    const active = this.activeBands()
    const soloBand = this.soloId !== null ? (this.bands.find((b) => b.id === this.soloId) ?? null) : null
    const ms = active.some((b) => b.channel !== 'stereo')

    let tail: AudioNode = this.inputGain
    if (ms) {
      this.encode()
      let midTail: AudioNode = this.midIn
      let sideTail: AudioNode = this.sideIn

      for (const band of active) {
        const groups: BiquadFilterNode[][] = []
        if (band.channel !== 'side') {
          const nodes = this.createBandNodes(band)
          for (const n of nodes) {
            midTail.connect(n)
            midTail = n
          }
          groups.push(nodes)
        }
        if (band.channel !== 'mid') {
          const nodes = this.createBandNodes(band)
          for (const n of nodes) {
            sideTail.connect(n)
            sideTail = n
          }
          groups.push(nodes)
        }
        this.bandNodes.set(band.id, groups)
      }

      // Soloing a single-channel band also isolates that channel, so you hear the
      // mid (or the side) alone rather than its filtered slice folded back in.
      this.setBusGain(this.midToL, soloBand?.channel === 'side' ? 0 : 1)
      this.setBusGain(this.midToR, soloBand?.channel === 'side' ? 0 : 1)
      this.setBusGain(this.sideToL, soloBand?.channel === 'mid' ? 0 : 1)
      this.setBusGain(this.sideToR, soloBand?.channel === 'mid' ? 0 : -1)

      this.decode(midTail, sideTail)
      tail = this.merger
    } else {
      for (const band of active) {
        const nodes = this.createBandNodes(band)
        for (const node of nodes) {
          tail.connect(node)
          tail = node
        }
        this.bandNodes.set(band.id, [nodes])
      }
    }

    if (soloBand) {
      for (const node of this.createSoloNodes(soloBand)) {
        tail.connect(node)
        tail = node
        this.soloNodes.push(node)
      }
    }

    tail.connect(this.outputGain)
    this.connectDetectors()
    this.topology = this.signature()
  }

  /** L/R -> mid, side. */
  private encode() {
    this.inputGain.connect(this.stereoIn)
    this.stereoIn.connect(this.splitter)
    this.splitter.connect(this.encMidL, 0)
    this.splitter.connect(this.encMidR, 1)
    this.splitter.connect(this.encSideL, 0)
    this.splitter.connect(this.encSideR, 1)
    this.encMidL.connect(this.midIn)
    this.encMidR.connect(this.midIn)
    this.encSideL.connect(this.sideIn)
    this.encSideR.connect(this.sideIn)
  }

  /** mid, side -> L/R. Merger inputs sum, so the four decode legs recombine there. */
  private decode(midTail: AudioNode, sideTail: AudioNode) {
    midTail.connect(this.midToL)
    midTail.connect(this.midToR)
    sideTail.connect(this.sideToL)
    sideTail.connect(this.sideToR)
    this.midToL.connect(this.merger, 0, 0)
    this.sideToL.connect(this.merger, 0, 0)
    this.midToR.connect(this.merger, 0, 1)
    this.sideToR.connect(this.merger, 0, 1)
  }

  private setBusGain(node: GainNode, v: number) {
    node.gain.setTargetAtTime(v, this.ctx.currentTime, 0.01)
  }

  private createBandNodes(band: Band): BiquadFilterNode[] {
    if (IS_CUT[band.type]) {
      const kind: BiquadFilterType = band.type === 'lowcut' ? 'highpass' : 'lowpass'
      return butterworthQs(band.slope / 6).map((q) => {
        const n = this.ctx.createBiquadFilter()
        n.type = kind
        n.frequency.value = this.safeFreq(band.freq)
        // Web Audio takes Q in dB for lowpass/highpass.
        n.Q.value = 20 * Math.log10(q)
        return n
      })
    }

    const n = this.ctx.createBiquadFilter()
    n.type = (
      {
        bell: 'peaking',
        lowshelf: 'lowshelf',
        highshelf: 'highshelf',
        notch: 'notch',
        bandpass: 'bandpass',
      } as Record<string, BiquadFilterType>
    )[band.type]
    n.frequency.value = this.safeFreq(band.freq)
    n.Q.value = band.q
    n.gain.value = band.gain + this.getDelta(band.id)
    return [n]
  }

  /** Solo listens through the region the band acts on. */
  private createSoloNodes(band: Band): BiquadFilterNode[] {
    const make = (type: BiquadFilterType, q: number) => {
      const n = this.ctx.createBiquadFilter()
      n.type = type
      n.frequency.value = this.safeFreq(band.freq)
      n.Q.value = q
      return n
    }
    switch (band.type) {
      case 'lowcut':
      case 'lowshelf':
        return [make('lowpass', 0)]
      case 'highcut':
      case 'highshelf':
        return [make('highpass', 0)]
      default:
        return [make('bandpass', Math.max(band.q, 0.7))]
    }
  }

  private safeFreq(f: number) {
    return Math.min(Math.max(f, 10), this.sampleRate / 2 - 1)
  }

  /** Push band values into the graph, rebuilding only when the topology changed. */
  setBands(bands: Band[]) {
    this.bands = bands
    this.syncDetectors(bands)

    if (this.signature() !== this.topology) {
      this.rebuild()
      return
    }

    const t = this.ctx.currentTime
    for (const band of bands) {
      const groups = this.bandNodes.get(band.id)
      if (!groups) continue
      for (const nodes of groups) {
        if (IS_CUT[band.type]) {
          for (const n of nodes) n.frequency.setTargetAtTime(this.safeFreq(band.freq), t, 0.008)
        } else {
          const n = nodes[0]
          n.frequency.setTargetAtTime(this.safeFreq(band.freq), t, 0.008)
          n.Q.setTargetAtTime(band.q, t, 0.008)
          // A dynamic band's gain belongs to the pump loop; don't fight it here.
          if (!this.isDynamic(band)) n.gain.setTargetAtTime(band.gain, t, 0.008)
        }
      }
    }

    const soloBand = bands.find((b) => b.id === this.soloId)
    if (soloBand) {
      for (const n of this.soloNodes) {
        n.frequency.setTargetAtTime(this.safeFreq(soloBand.freq), t, 0.008)
      }
    }
  }

  private isDynamic(band: Band): boolean {
    return band.dynamic && band.enabled && USES_GAIN[band.type]
  }

  // --- dynamics ----------------------------------------------------------

  /**
   * A dynamic band should react to the signal it actually filters, so a mid- or
   * side-only band listens to its own bus — but only while the M/S graph exists.
   */
  private detectorSource(band: Band | undefined): AudioNode {
    if (!band || band.channel === 'stereo') return this.inputGain
    if (!this.activeBands().some((b) => b.channel !== 'stereo')) return this.inputGain
    return band.channel === 'mid' ? this.midIn : this.sideIn
  }

  /** (Re)wire every detector's input. Safe to call repeatedly — edges are deduped. */
  private connectDetectors() {
    for (const [id, d] of this.detectors) {
      const src = this.detectorSource(this.bands.find((b) => b.id === id))
      if (d.source && d.source !== src) {
        try {
          d.source.disconnect(d.filter)
        } catch {
          // A rebuild already tore that edge down; disconnecting a dead one throws.
        }
      }
      src.connect(d.filter)
      d.source = src
    }
  }

  /** Create/remove/retune the sidechain detectors so they track the dynamic bands. */
  private syncDetectors(bands: Band[]) {
    const wanted = new Set(bands.filter((b) => this.isDynamic(b)).map((b) => b.id))

    for (const [id, d] of this.detectors) {
      if (wanted.has(id)) continue
      try {
        d.source?.disconnect(d.filter)
      } catch {
        // Edge already gone with the last rebuild.
      }
      d.filter.disconnect()
      d.analyser.disconnect()
      this.detectors.delete(id)
      this.deltas.delete(id)
    }

    for (const band of bands) {
      if (!this.isDynamic(band)) continue
      let d = this.detectors.get(band.id)
      if (!d) {
        const filter = this.ctx.createBiquadFilter()
        const analyser = this.ctx.createAnalyser()
        analyser.fftSize = 2048
        filter.connect(analyser)
        const source = this.detectorSource(band)
        source.connect(filter)
        d = {
          filter, analyser, buf: new Float32Array(analyser.fftSize), env: 0, level: -100, source,
        }
        this.detectors.set(band.id, d)
      }
      // Listen to the slice of spectrum the band acts on.
      const t = this.ctx.currentTime
      if (band.type === 'lowshelf') {
        d.filter.type = 'lowpass'
        d.filter.Q.setTargetAtTime(0, t, 0.01)
      } else if (band.type === 'highshelf') {
        d.filter.type = 'highpass'
        d.filter.Q.setTargetAtTime(0, t, 0.01)
      } else {
        d.filter.type = 'bandpass'
        d.filter.Q.setTargetAtTime(Math.max(band.q, 0.5), t, 0.01)
      }
      d.filter.frequency.setTargetAtTime(this.safeFreq(band.freq), t, 0.01)
    }
  }

  /**
   * Control-rate dynamics: measure each dynamic band's level, smooth it with the
   * band's attack/release, and drive its filter gain. Running at frame rate rather
   * than audio rate keeps this in plain Web Audio nodes; the cost is that attack
   * times below roughly a frame (~16 ms) are floored by the update interval.
   */
  private pump(now: number) {
    this.pumpHandle = requestAnimationFrame(this.pump)
    const dt = Math.min((now - this.lastPump) / 1000, 0.1)
    this.lastPump = now
    if (dt <= 0) return

    const t = this.ctx.currentTime

    for (const band of this.bands) {
      const d = this.detectors.get(band.id)
      if (!d || !this.isDynamic(band)) continue

      d.analyser.getFloatTimeDomainData(d.buf)
      d.level = rmsDb(d.buf)

      const step = dynamicStep(band, d.level, d.env, dt)
      d.env = step.env
      this.deltas.set(band.id, step.delta)

      for (const nodes of this.bandNodes.get(band.id) ?? []) {
        nodes[0]?.gain.setTargetAtTime(band.gain + step.delta, t, 0.01)
      }
    }
  }

  setSolo(id: number | null) {
    this.soloId = id
    this.rebuild()
  }

  setBypass(on: boolean) {
    this.bypassed = on
    this.rebuild()
  }

  setOutputGain(db: number) {
    this.outputGain.gain.setTargetAtTime(Math.pow(10, db / 20), this.ctx.currentTime, 0.02)
  }

  // --- transport ---------------------------------------------------------

  async loadFile(file: File) {
    this.emit('loading')
    this.stop()
    const data = await file.arrayBuffer()
    this.buffer = await this.ctx.decodeAudioData(data)
    this.fileName = file.name
    this.pausedAt = 0
    this.emit('ready')
  }

  async play() {
    if (!this.buffer || this.playing) return
    if (this.ctx.state === 'suspended') await this.ctx.resume()

    const src = this.ctx.createBufferSource()
    src.buffer = this.buffer
    src.loop = this.loop
    src.connect(this.inputGain)
    src.onended = () => {
      if (this.source === src && !this.loop) {
        this.playing = false
        this.pausedAt = 0
        this.source = null
        this.emit('ready')
        this.listeners.onEnded?.()
      }
    }
    src.start(0, this.pausedAt % this.buffer.duration)
    this.source = src
    this.startedAt = this.ctx.currentTime
    this.playing = true
    this.emit('playing')
  }

  pause() {
    if (!this.playing) return
    const pos = this.position
    this.stopSource()
    this.pausedAt = pos
    this.playing = false
    this.emit('ready')
  }

  stop() {
    this.stopSource()
    this.pausedAt = 0
    this.playing = false
    if (this.buffer) this.emit('ready')
  }

  seek(seconds: number) {
    const was = this.playing
    this.stopSource()
    this.playing = false
    this.pausedAt = Math.max(0, Math.min(seconds, this.duration))
    if (was) void this.play()
  }

  setLoop(on: boolean) {
    this.loop = on
    if (this.source) this.source.loop = on
  }

  private stopSource() {
    if (!this.source) return
    this.source.onended = null
    try {
      this.source.stop()
    } catch {
      /* already stopped */
    }
    this.source.disconnect()
    this.source = null
  }

  private emit(state: EngineState) {
    this.listeners.onStateChange?.(state)
  }

  dispose() {
    cancelAnimationFrame(this.pumpHandle)
    this.stopSource()
    void this.ctx.close()
  }
}
