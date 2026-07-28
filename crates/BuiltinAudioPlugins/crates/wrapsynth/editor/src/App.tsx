import { useEffect, useMemo, useState, type CSSProperties } from 'react'
import { connectBridge, postParam, type SynthParams, type Waveform } from './bridge'

const defaults: SynthParams = {
  power: true,
  oscAWave: 'saw',
  oscAPosition: 0.18,
  oscALevel: 0.78,
  oscBWave: 'square',
  oscBPosition: 0.42,
  oscBLevel: 0.38,
  oscBSemitones: 0,
  oscBDetuneCents: 7,
  unison: 3,
  unisonDetuneCents: 14,
  stereoWidth: 0.72,
  subLevel: 0.16,
  noiseLevel: 0.025,
  cutoffHz: 6400,
  resonance: 0.18,
  filterDrive: 0.12,
  attackMs: 8,
  decayMs: 220,
  sustain: 0.72,
  releaseMs: 420,
  masterDb: -8,
}

const waveToWire: Record<Waveform, number> = { saw: 0, square: 1, triangle: 2, sine: 3 }

type KnobProps = {
  label: string
  value: number
  min: number
  max: number
  step?: number
  display?: (value: number) => string
  onChange: (value: number) => void
}

function Knob({ label, value, min, max, step = 0.01, display, onChange }: KnobProps) {
  const ratio = (value - min) / (max - min)
  const angle = -135 + ratio * 270
  return (
    <label className="knob-control">
      <span className="knob-label">{label}</span>
      <span className="knob" style={{ '--knob-angle': `${angle}deg` } as CSSProperties}>
        <span className="knob-cap"><i /></span>
        <input
          aria-label={label}
          type="range"
          min={min}
          max={max}
          step={step}
          value={value}
          onChange={(event) => onChange(Number(event.target.value))}
        />
      </span>
      <output>{display ? display(value) : value.toFixed(step < 0.1 ? 2 : 0)}</output>
    </label>
  )
}

function WaveDisplay({ wave, position }: { wave: Waveform; position: number }) {
  const path = useMemo(() => {
    const points: string[] = []
    for (let index = 0; index <= 128; index += 1) {
      const phase = index / 128
      const warped = Math.pow(phase, 0.45 + position * 1.55)
      let sample = 0
      if (wave === 'saw') sample = warped * 2 - 1
      if (wave === 'square') sample = warped < 0.5 ? 1 : -1
      if (wave === 'triangle') sample = 1 - 4 * Math.abs(warped - 0.5)
      if (wave === 'sine') sample = Math.sin(Math.PI * 2 * warped)
      points.push(`${index === 0 ? 'M' : 'L'} ${phase * 300} ${54 - sample * 37}`)
    }
    return points.join(' ')
  }, [position, wave])
  return (
    <svg className="wave-display" viewBox="0 0 300 108" preserveAspectRatio="none" aria-label={`${wave} wavetable`}>
      <g className="grid"><path d="M0 27H300M0 54H300M0 81H300M75 0V108M150 0V108M225 0V108" /></g>
      <path className="wave-line" d={path} />
    </svg>
  )
}

function Oscillator({
  id,
  wave,
  position,
  level,
  onWave,
  onPosition,
  onLevel,
  children,
}: {
  id: 'A' | 'B'
  wave: Waveform
  position: number
  level: number
  onWave: (wave: Waveform) => void
  onPosition: (value: number) => void
  onLevel: (value: number) => void
  children?: React.ReactNode
}) {
  return (
    <section className="module oscillator">
      <header><strong>Oscillator {id}</strong><small>Wavetable</small></header>
      <WaveDisplay wave={wave} position={position} />
      <div className="wave-tabs">
        {(['saw', 'square', 'triangle', 'sine'] as Waveform[]).map((option) => (
          <button
            type="button"
            key={option}
            className={wave === option ? 'active' : ''}
            onClick={() => onWave(option)}
          >{option.slice(0, 3).toUpperCase()}</button>
        ))}
      </div>
      <div className="knob-row">
        <Knob label="POSITION" value={position} min={0} max={1} display={(v) => `${Math.round(v * 100)}%`} onChange={onPosition} />
        <Knob label="LEVEL" value={level} min={0} max={1} display={(v) => `${Math.round(v * 100)}%`} onChange={onLevel} />
        {children}
      </div>
    </section>
  )
}

function App() {
  const [params, setParams] = useState(defaults)
  const [connected, setConnected] = useState(false)

  useEffect(() => connectBridge(setParams, setConnected), [])

  const change = <K extends keyof SynthParams>(id: K, value: SynthParams[K], wire?: number) => {
    setParams((current) => ({ ...current, [id]: value }))
    postParam(id, wire ?? Number(value))
  }

  const freq = (value: number) => value >= 1000 ? `${(value / 1000).toFixed(1)}k` : `${Math.round(value)}`
  const time = (value: number) => value >= 1000 ? `${(value / 1000).toFixed(1)}s` : `${Math.round(value)}ms`
  const envelopePath = useMemo(() => {
    const scaledTime = (value: number, maximum: number) =>
      Math.log10(1 + value) / Math.log10(1 + maximum)
    const attackX = 8 + scaledTime(params.attackMs, 5000) * 58
    const decayX = attackX + 34 + scaledTime(params.decayMs, 5000) * 48
    const releaseStart = 220
    const releaseX = releaseStart + 28 + scaledTime(params.releaseMs, 8000) * 44
    const sustainY = 78 - params.sustain * 62
    return {
      line: `M8 78 L${attackX} 8 L${decayX} ${sustainY} H${releaseStart} L${releaseX} 78`,
      fill: `M8 78 L${attackX} 8 L${decayX} ${sustainY} H${releaseStart} L${releaseX} 78 Z`,
    }
  }, [params.attackMs, params.decayMs, params.releaseMs, params.sustain])

  return (
    <main className={params.power ? '' : 'powered-off'}>
      <div className="instrument-shell">
        <header className="topbar">
          <div className="brand"><span>W</span><div><strong>WrapSynth</strong><small>Wavetable instrument</small></div></div>
          <div className="patch"><span>Patch</span><strong>Init</strong></div>
          <div className="status"><i className={connected ? 'online' : ''} /><span>{connected ? 'Connected' : 'Preview'}</span></div>
          <button className="power" type="button" aria-pressed={params.power} onClick={() => change('power', !params.power, params.power ? 0 : 1)}>Power</button>
        </header>

        <div className="workspace">
          <div className="oscillators">
          <Oscillator
            id="A"
            wave={params.oscAWave}
            position={params.oscAPosition}
            level={params.oscALevel}
            onWave={(wave) => change('oscAWave', wave, waveToWire[wave])}
            onPosition={(value) => change('oscAPosition', value)}
            onLevel={(value) => change('oscALevel', value)}
          >
            <Knob label="UNISON" value={params.unison} min={1} max={7} step={1} onChange={(value) => change('unison', value)} />
            <Knob label="DETUNE" value={params.unisonDetuneCents} min={0} max={50} step={1} display={(v) => `${v.toFixed(0)}ct`} onChange={(value) => change('unisonDetuneCents', value)} />
          </Oscillator>
          <Oscillator
            id="B"
            wave={params.oscBWave}
            position={params.oscBPosition}
            level={params.oscBLevel}
            onWave={(wave) => change('oscBWave', wave, waveToWire[wave])}
            onPosition={(value) => change('oscBPosition', value)}
            onLevel={(value) => change('oscBLevel', value)}
          >
            <Knob label="SEMITONE" value={params.oscBSemitones} min={-24} max={24} step={1} display={(v) => `${v > 0 ? '+' : ''}${v}`} onChange={(value) => change('oscBSemitones', value)} />
            <Knob label="FINE" value={params.oscBDetuneCents} min={-50} max={50} step={1} display={(v) => `${v > 0 ? '+' : ''}${v}ct`} onChange={(value) => change('oscBDetuneCents', value)} />
          </Oscillator>
          </div>

          <aside className="right-stack">
            <section className="module filter">
            <header><strong>Filter</strong><small>Low-pass</small></header>
            <div className="filter-plot"><svg viewBox="0 0 240 92" preserveAspectRatio="none"><path className="grid" d="M0 23H240M0 46H240M0 69H240M60 0V92M120 0V92M180 0V92" /><path className="filter-fill" d={`M0 12 H${Math.min(218, 25 + Math.log10(params.cutoffHz / 40) / Math.log10(500) * 185)} Q225 18 239 88 V92 H0Z`} /><path className="filter-line" d={`M0 12 H${Math.min(218, 25 + Math.log10(params.cutoffHz / 40) / Math.log10(500) * 185)} Q225 18 239 88`} /></svg></div>
            <div className="knob-row">
              <Knob label="CUTOFF" value={params.cutoffHz} min={40} max={20000} step={1} display={freq} onChange={(value) => change('cutoffHz', value)} />
              <Knob label="RESO" value={params.resonance} min={0} max={0.95} display={(v) => `${Math.round(v * 100)}%`} onChange={(value) => change('resonance', value)} />
              <Knob label="DRIVE" value={params.filterDrive} min={0} max={1} display={(v) => `${Math.round(v * 100)}%`} onChange={(value) => change('filterDrive', value)} />
            </div>
          </section>

            <section className="module source-mix">
            <header><strong>Output</strong><small>Sources and level</small></header>
            <div className="knob-row">
              <Knob label="SUB" value={params.subLevel} min={0} max={1} display={(v) => `${Math.round(v * 100)}%`} onChange={(value) => change('subLevel', value)} />
              <Knob label="NOISE" value={params.noiseLevel} min={0} max={1} display={(v) => `${Math.round(v * 100)}%`} onChange={(value) => change('noiseLevel', value)} />
              <Knob label="WIDTH" value={params.stereoWidth} min={0} max={1} display={(v) => `${Math.round(v * 100)}%`} onChange={(value) => change('stereoWidth', value)} />
              <Knob label="MASTER" value={params.masterDb} min={-24} max={3} step={0.1} display={(v) => `${v.toFixed(1)}dB`} onChange={(value) => change('masterDb', value)} />
            </div>
            </section>
          </aside>
        </div>

        <section className="modulation">
          <div className="env-head"><strong>Amp envelope</strong><small>ENV 1</small></div>
          <div className="envelope-plot"><svg viewBox="0 0 320 86" preserveAspectRatio="none"><path className="grid" d="M0 43H320M80 0V86M160 0V86M240 0V86" /><path className="env-fill" d={envelopePath.fill} /><path className="env-line" d={envelopePath.line} /></svg></div>
          <div className="env-controls">
          <Knob label="ATTACK" value={params.attackMs} min={0.5} max={5000} step={0.5} display={time} onChange={(value) => change('attackMs', value)} />
          <Knob label="DECAY" value={params.decayMs} min={1} max={5000} step={1} display={time} onChange={(value) => change('decayMs', value)} />
          <Knob label="SUSTAIN" value={params.sustain} min={0} max={1} display={(v) => `${Math.round(v * 100)}%`} onChange={(value) => change('sustain', value)} />
          <Knob label="RELEASE" value={params.releaseMs} min={5} max={8000} step={1} display={time} onChange={(value) => change('releaseMs', value)} />
          </div>
        </section>

        <footer><span>16 voices · 2 oscillators · 7× unison</span><span>MIDI follows track input</span></footer>
      </div>
    </main>
  )
}

export default App
