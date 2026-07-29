import { useEffect, useRef, useState, type CSSProperties } from 'react'
import {
  MODELS,
  MODEL_LABEL,
  MODEL_WIRE,
  connectBridge,
  defaults,
  postParam,
  type MeterFrame,
  type ZcompParams,
} from './bridge'
import { CurveDisplay } from './CurveDisplay'
import {
  CIRCUIT_INFO,
  DEVICE_HEIGHT,
  DEVICE_WIDTH,
  MAX_SCALE,
} from './device'
import { Knob } from './Knob'
import { MeterBay } from './MeterBay'
import { clamp, type MeterMode } from './meter'
import { PARAM_SPECS, releaseIsProgrammed, sanitizeParams } from './params'
import { PresetControl } from './PresetControl'
import { FACTORY_PRESETS, matchingPresetIndex, postAllParams } from './presets'

function formatMs(value: number) {
  return value < 10 ? value.toFixed(2) : String(Math.round(value))
}
function formatDb(value: number) {
  return value.toFixed(1)
}
function formatPct(value: number) {
  return String(Math.round(value))
}
function formatHz(value: number) {
  return String(Math.round(value))
}
function formatRatio(value: number) {
  return value.toFixed(1)
}

/**
 * Scale the fixed-size faceplate to the client rectangle the native host gave
 * the browser.
 *
 * The panel owns its own layout in design pixels; this hook owns nothing but
 * the scale factor. Nothing inside the device measures the window, so no
 * region can end up sized by a circular percentage chain.
 */
function useDeviceScale() {
  const stageRef = useRef<HTMLDivElement>(null)
  const [scale, setScale] = useState(1)

  useEffect(() => {
    const stage = stageRef.current
    if (!stage) return
    const fit = (width: number, height: number) => {
      if (width <= 0 || height <= 0) return
      setScale(
        clamp(Math.min(width / DEVICE_WIDTH, height / DEVICE_HEIGHT), 0.2, MAX_SCALE),
      )
    }
    fit(stage.clientWidth, stage.clientHeight)
    const observer = new ResizeObserver((entries) => {
      const box = entries[0]?.contentRect
      if (box) fit(box.width, box.height)
    })
    observer.observe(stage)
    return () => observer.disconnect()
  }, [])

  return { stageRef, scale }
}

function App() {
  const [params, setParams] = useState<ZcompParams>(defaults)
  const [connected, setConnected] = useState(false)
  const [meterLive, setMeterLive] = useState(false)
  const [meterMode, setMeterMode] = useState<MeterMode>('reduction')
  const metersRef = useRef<MeterFrame | null>(null)
  // Mirrors `meterLive` so the meter callback can stay out of React entirely
  // until the first frame arrives — telemetry runs at frame rate.
  const meterLiveRef = useRef(false)
  const [preset, setPreset] = useState<number | null>(0)
  const { stageRef, scale } = useDeviceScale()

  useEffect(
    () =>
      connectBridge(
        (next) => {
          const clean = sanitizeParams(next)
          setParams(clean)
          setPreset(matchingPresetIndex(clean))
        },
        (isConnected) => {
          setConnected(isConnected)
          if (!isConnected) {
            metersRef.current = null
            meterLiveRef.current = false
            setMeterLive(false)
          }
        },
        (frame) => {
          metersRef.current = frame
          if (meterLiveRef.current) return
          meterLiveRef.current = true
          setMeterLive(true)
        },
      ),
    [],
  )

  const change = <K extends keyof ZcompParams>(
    id: K,
    value: ZcompParams[K],
    wire?: number,
  ) => {
    setParams((current) => sanitizeParams({ ...current, [id]: value }))
    setPreset(null)
    postParam(id, wire ?? Number(value))
  }

  const loadPreset = (index: number) => {
    const wrapped =
      ((index % FACTORY_PRESETS.length) + FACTORY_PRESETS.length) %
      FACTORY_PRESETS.length
    const next = sanitizeParams({ ...FACTORY_PRESETS[wrapped]!.params })
    setParams(next)
    setPreset(wrapped)
    postAllParams(next)
  }

  const programmedRelease = releaseIsProgrammed(params)
  const circuit = CIRCUIT_INFO[params.model]
  const metersActive = connected && meterLive

  return (
    <div className="stage" ref={stageRef}>
      <div
        className={`device${params.power ? '' : ' is-off'}`}
        data-model={params.model}
        style={
          {
            width: `${DEVICE_WIDTH}px`,
            height: `${DEVICE_HEIGHT}px`,
            transform: `translate(-50%, -50%) scale(${scale})`,
          } as CSSProperties
        }
      >
        <div className="ear" aria-hidden="true">
          <span className="screw" />
          <span className="screw" />
        </div>

        <div className="face">
          <header className="head">
            <div className="identity">
              <div className="nameplate">
                <span className="mark">Z</span>
                <div className="titles">
                  <strong>Z—COMP</strong>
                  <em>Multi-Circuit Dynamics</em>
                </div>
              </div>
              <div className="maker">
                <span>Futureboard</span>
                <span
                  className={`link${connected ? ' is-live' : ''}`}
                  title={connected ? 'Linked to the host' : 'No host instance bound'}
                >
                  <i />
                  {connected ? 'Linked' : 'Standby'}
                </span>
              </div>
              <PresetControl
                preset={preset}
                names={FACTORY_PRESETS.map((entry) => entry.name)}
                onChange={loadPreset}
                onPrevious={() => loadPreset((preset ?? 0) - 1)}
                onNext={() => loadPreset((preset ?? -1) + 1)}
              />

              {/* Silkscreen of the actual chain in `zcomp::dsp`. */}
              <ol className="signal-path" aria-label="Signal path">
                <li>Sidechain HPF</li>
                <li>Detector</li>
                <li>Gain cell</li>
                <li>Colour</li>
              </ol>
            </div>

            <MeterBay
              metersRef={metersRef}
              live={metersActive}
              mode={meterMode}
              onClearOutClip={() => {
                const frame = metersRef.current
                if (frame) metersRef.current = { ...frame, outClip: false }
              }}
            />

            <CurveDisplay params={params} metersRef={metersRef} live={metersActive} />

            <div className="master">
              <div className="switchbank" role="group" aria-label="Meter mode">
                <button
                  type="button"
                  className={meterMode === 'reduction' ? 'is-on' : undefined}
                  aria-pressed={meterMode === 'reduction'}
                  title="Meter shows gain reduction"
                  onClick={() => setMeterMode('reduction')}
                >
                  GR
                </button>
                <button
                  type="button"
                  className={meterMode === 'output' ? 'is-on' : undefined}
                  aria-pressed={meterMode === 'output'}
                  title="Meter shows output level, 0 VU = −18 dBFS"
                  onClick={() => setMeterMode('output')}
                >
                  +4
                </button>
              </div>

              <button
                type="button"
                className={`rocker${params.power ? ' is-on' : ''}`}
                role="switch"
                aria-checked={params.power}
                aria-label="Power"
                title={params.power ? 'Engaged — click to bypass' : 'Bypassed'}
                onClick={() => change('power', !params.power, params.power ? 0 : 1)}
              >
                <span className="lamp" />
                <span className="legend">{params.power ? 'In' : 'Byp'}</span>
              </button>
              <span className="master-tag">Power</span>
            </div>
          </header>

          <section className="circuit" aria-label="Circuit">
            <span className="bay-label">Circuit</span>
            <div className="bank" role="radiogroup" aria-label="Circuit model">
              {MODELS.map((model) => (
                <button
                  key={model}
                  type="button"
                  className={`lamp-button${params.model === model ? ' is-on' : ''}`}
                  role="radio"
                  aria-checked={params.model === model}
                  onClick={() => change('model', model, MODEL_WIRE[model])}
                >
                  <i />
                  {MODEL_LABEL[model]}
                </button>
              ))}
            </div>

            <p className="engraved" aria-live="polite">
              <span>{circuit.topology}</span>
            </p>

            <div className="modes">
              <button
                type="button"
                className={`lamp-button${params.autoRelease ? ' is-on' : ''}`}
                aria-pressed={params.autoRelease}
                title={
                  params.model === 'ssl'
                    ? 'Dual time-constant automatic release'
                    : 'Adds program dependence to this circuit’s release'
                }
                onClick={() =>
                  change('autoRelease', !params.autoRelease, params.autoRelease ? 0 : 1)
                }
              >
                <i />
                Auto Rel
              </button>
              <button
                type="button"
                className={`lamp-button alert${params.scListen ? ' is-on' : ''}`}
                aria-pressed={params.scListen}
                title="Audition the filtered sidechain the detector hears"
                onClick={() =>
                  change('scListen', !params.scListen, params.scListen ? 0 : 1)
                }
              >
                <i />
                SC Listen
              </button>
            </div>
          </section>

          <section className="controls" aria-label="Controls">
            <div className="row main">
              <Knob
                spec={PARAM_SPECS.thresholdDb}
                scale={PARAM_SPECS.thresholdDb.scale}
                value={params.thresholdDb}
                display={formatDb}
                onChange={(value) => change('thresholdDb', value)}
              />
              <Knob
                spec={PARAM_SPECS.ratio}
                scale={PARAM_SPECS.ratio.scale}
                value={params.ratio}
                display={formatRatio}
                onChange={(value) => change('ratio', value)}
              />
              <Knob
                spec={PARAM_SPECS.attackMs}
                scale={PARAM_SPECS.attackMs.scale}
                value={params.attackMs}
                display={formatMs}
                onChange={(value) => change('attackMs', value)}
              />
              <Knob
                spec={PARAM_SPECS.releaseMs}
                scale={PARAM_SPECS.releaseMs.scale}
                value={params.releaseMs}
                display={formatMs}
                onChange={(value) => change('releaseMs', value)}
                disabled={programmedRelease}
                disabledReadout="Auto"
              />
              <Knob
                spec={PARAM_SPECS.makeupDb}
                scale={PARAM_SPECS.makeupDb.scale}
                value={params.makeupDb}
                display={formatDb}
                onChange={(value) => change('makeupDb', value)}
              />
            </div>

            <div className="row trim">
              <Knob
                size="sm"
                spec={PARAM_SPECS.kneeDb}
                scale={PARAM_SPECS.kneeDb.scale}
                value={params.kneeDb}
                display={formatDb}
                onChange={(value) => change('kneeDb', value)}
              />
              <Knob
                size="sm"
                spec={PARAM_SPECS.sidechainHpfHz}
                scale={PARAM_SPECS.sidechainHpfHz.scale}
                value={params.sidechainHpfHz}
                display={formatHz}
                onChange={(value) => change('sidechainHpfHz', value)}
              />
              <Knob
                size="sm"
                spec={PARAM_SPECS.stereoLink}
                scale={PARAM_SPECS.stereoLink.scale}
                value={params.stereoLink}
                display={formatPct}
                onChange={(value) => change('stereoLink', value)}
              />
              <Knob
                size="sm"
                spec={PARAM_SPECS.color}
                scale={PARAM_SPECS.color.scale}
                value={params.color}
                display={formatPct}
                onChange={(value) => change('color', value)}
              />
              <Knob
                size="sm"
                spec={PARAM_SPECS.mix}
                scale={PARAM_SPECS.mix.scale}
                value={params.mix}
                display={formatPct}
                onChange={(value) => change('mix', value)}
              />
            </div>
          </section>
        </div>

        <div className="ear" aria-hidden="true">
          <span className="screw" />
          <span className="screw" />
        </div>
      </div>
    </div>
  )
}

export default App
