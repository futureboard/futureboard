import { useEffect, useState, type CSSProperties } from 'react'
import {
  MODEL_LABEL,
  MODEL_WIRE,
  MODELS,
  connectBridge,
  defaults,
  postParam,
  type CompModel,
  type MeterFrame,
  type ZcompParams,
} from './bridge'

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
  const ratio = (value - min) / Math.max(max - min, 1e-9)
  const angle = -135 + ratio * 270
  return (
    <label className="knob-control">
      <span className="knob-label">{label}</span>
      <span className="knob" style={{ '--knob-angle': `${angle}deg` } as CSSProperties}>
        <span className="knob-cap">
          <i />
        </span>
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
      <output>{display ? display(value) : value.toFixed(step < 1 ? 1 : 0)}</output>
    </label>
  )
}

function LevelBar({
  label,
  peak,
  rms,
  clip,
}: {
  label: string
  peak: number
  rms: number
  clip: boolean
}) {
  const toDb = (linear: number) => 20 * Math.log10(Math.max(linear, 1e-9))
  const peakDb = toDb(peak)
  const rmsDb = toDb(rms)
  const map = (db: number) => Math.max(0, Math.min(1, (db + 60) / 60))
  return (
    <div className={`level-bar ${clip ? 'clip' : ''}`}>
      <span>{label}</span>
      <div className="level-track">
        <i className="rms" style={{ height: `${map(rmsDb) * 100}%` }} />
        <i className="peak" style={{ height: `${map(peakDb) * 100}%` }} />
      </div>
      <output>{peakDb.toFixed(0)}</output>
    </div>
  )
}

function GrMeter({ db }: { db: number }) {
  const amount = Math.max(0, Math.min(1, db / 24))
  const ticks = [0, 3, 6, 9, 12, 18, 24]
  return (
    <div className="gr-meter" aria-label={`Gain reduction ${db.toFixed(1)} dB`}>
      <header>
        <strong>GR</strong>
        <output>{db.toFixed(1)} dB</output>
      </header>
      <div className="gr-face">
        <div className="gr-fill" style={{ width: `${amount * 100}%` }} />
        <div className="gr-ticks">
          {ticks.map((tick) => (
            <span key={tick} style={{ left: `${(tick / 24) * 100}%` }}>
              {tick}
            </span>
          ))}
        </div>
      </div>
    </div>
  )
}

function App() {
  const [params, setParams] = useState<ZcompParams>(defaults)
  const [connected, setConnected] = useState(false)
  const [meters, setMeters] = useState<MeterFrame>({
    inPeak: 0,
    inRms: 0,
    outPeak: 0,
    outRms: 0,
    gainReductionDb: 0,
    inClip: false,
    outClip: false,
  })

  useEffect(() => connectBridge(setParams, setConnected, setMeters), [])

  const change = <K extends keyof ZcompParams>(
    id: K,
    value: ZcompParams[K],
    wire?: number,
  ) => {
    setParams((current) => ({ ...current, [id]: value }))
    postParam(id, wire ?? Number(value))
  }

  const setModel = (model: CompModel) => {
    change('model', model, MODEL_WIRE[model])
  }

  const ms = (value: number) =>
    value < 10 ? `${value.toFixed(2)} ms` : `${Math.round(value)} ms`
  const db = (value: number) => `${value.toFixed(1)} dB`
  const pct = (value: number) => `${Math.round(value)}%`
  const hz = (value: number) => `${Math.round(value)} Hz`
  const ratio = (value: number) => `${value.toFixed(1)}:1`

  return (
    <main className={`shell ${params.power ? '' : 'powered-off'} model-${params.model}`}>
      <header className="topbar">
        <div className="brand">
          <span>Z</span>
          <div>
            <strong>Z-Comp</strong>
            <small>Ultimate Dynamics</small>
          </div>
        </div>
        <div className="model-strip" role="tablist" aria-label="Compressor model">
          {MODELS.map((model) => (
            <button
              key={model}
              type="button"
              role="tab"
              aria-selected={params.model === model}
              className={params.model === model ? 'active' : ''}
              onClick={() => setModel(model)}
            >
              {MODEL_LABEL[model]}
            </button>
          ))}
        </div>
        <div className="top-actions">
          <button
            type="button"
            className={`toggle ${params.autoRelease ? 'on' : ''}`}
            onClick={() => change('autoRelease', !params.autoRelease, params.autoRelease ? 0 : 1)}
          >
            Auto Rel
          </button>
          <button
            type="button"
            className={`power ${params.power ? 'on' : ''}`}
            onClick={() => change('power', !params.power, params.power ? 0 : 1)}
          >
            {params.power ? 'On' : 'Off'}
          </button>
          <span className={`link-dot ${connected ? 'live' : ''}`} title={connected ? 'Host linked' : 'Preview'} />
        </div>
      </header>

      <section className="workspace">
        <aside className="meters">
          <LevelBar label="IN" peak={meters.inPeak} rms={meters.inRms} clip={meters.inClip} />
          <LevelBar label="OUT" peak={meters.outPeak} rms={meters.outRms} clip={meters.outClip} />
        </aside>

        <div className="center">
          <GrMeter db={meters.gainReductionDb} />
          <p className="model-blurb">
            {params.model === 'comp2500' && 'VCA feed-forward · soft dual knee · THD colour'}
            {params.model === 'distressor' && 'Aggressive detector · British grit · hard ratios'}
            {params.model === 'avalon' && 'Class-A optical leveling · slow musical recovery'}
            {params.model === 'ssl' && 'Bus glue · program auto-release · soft knee'}
          </p>
          <div className="knob-row primary">
            <Knob
              label="THRESH"
              value={params.thresholdDb}
              min={-60}
              max={0}
              step={0.1}
              display={db}
              onChange={(value) => change('thresholdDb', value)}
            />
            <Knob
              label="RATIO"
              value={params.ratio}
              min={1}
              max={20}
              step={0.1}
              display={ratio}
              onChange={(value) => change('ratio', value)}
            />
            <Knob
              label="ATTACK"
              value={params.attackMs}
              min={0.01}
              max={120}
              step={0.01}
              display={ms}
              onChange={(value) => change('attackMs', value)}
            />
            <Knob
              label="RELEASE"
              value={params.releaseMs}
              min={10}
              max={2500}
              step={1}
              display={ms}
              onChange={(value) => change('releaseMs', value)}
            />
            <Knob
              label="MAKEUP"
              value={params.makeupDb}
              min={-24}
              max={24}
              step={0.1}
              display={db}
              onChange={(value) => change('makeupDb', value)}
            />
          </div>
        </div>
      </section>

      <section className="secondary">
        <Knob
          label="KNEE"
          value={params.kneeDb}
          min={0}
          max={24}
          step={0.1}
          display={db}
          onChange={(value) => change('kneeDb', value)}
        />
        <Knob
          label="MIX"
          value={params.mix}
          min={0}
          max={100}
          step={1}
          display={pct}
          onChange={(value) => change('mix', value)}
        />
        <Knob
          label="SC HPF"
          value={params.sidechainHpfHz}
          min={20}
          max={500}
          step={1}
          display={hz}
          onChange={(value) => change('sidechainHpfHz', value)}
        />
        <Knob
          label="LINK"
          value={params.stereoLink}
          min={0}
          max={100}
          step={1}
          display={pct}
          onChange={(value) => change('stereoLink', value)}
        />
        <Knob
          label="COLOR"
          value={params.color}
          min={0}
          max={100}
          step={1}
          display={pct}
          onChange={(value) => change('color', value)}
        />
      </section>

      <footer>
        <span>Futureboard · Z-Comp</span>
        <span>{MODEL_LABEL[params.model]} circuit</span>
      </footer>
    </main>
  )
}

export default App
