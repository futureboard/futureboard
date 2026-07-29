import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
} from 'react'
import {
  MODEL_LABEL,
  MODEL_WIRE,
  MODELS,
  connectBridge,
  defaults,
  postParam,
  type MeterFrame,
  type ZcompParams,
} from './bridge'
import { MeterBay } from './MeterBay'
import { clamp, type MeterMode } from './meter'
import {
  MODEL_CIRCUIT,
  PARAM_SPECS,
  releaseIsProgrammed,
  sanitizeParams,
  type ParamSpec,
} from './params'
import { PresetControl } from './PresetControl'
import {
  FACTORY_PRESETS,
  matchingPresetIndex,
  postAllParams,
} from './presets'

const TRAVEL_PX = 240
const START_DEG = -240
const SWEEP_DEG = 300

type KnobProps = {
  spec: ParamSpec
  value: number
  display: (value: number) => string
  onChange: (value: number) => void
  size?: 'lg' | 'sm'
  disabled?: boolean
  disabledReadout?: string
}

function AluminumKnob({
  spec,
  value,
  display,
  onChange,
  size = 'lg',
  disabled = false,
  disabledReadout,
}: KnobProps) {
  const uid = useId().replace(/:/g, '')
  const gesture = useRef<{ y: number; norm: number } | null>(null)
  const [dragging, setDragging] = useState(false)
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState('')
  const inputRef = useRef<HTMLInputElement>(null)

  const { min, max, step, label, unit, defaultValue } = spec
  const norm = (value - min) / Math.max(max - min, 1e-9)
  const fromNorm = useCallback(
    (n: number) => {
      const raw = min + clamp(n, 0, 1) * (max - min)
      return clamp(Math.round(raw / step) * step, min, max)
    },
    [max, min, step],
  )

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus()
      inputRef.current?.select()
    }
  }, [editing])

  const pointerAngle = START_DEG + SWEEP_DEG * clamp(norm, 0, 1)

  const onPointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (disabled || editing || event.button !== 0) return
    gesture.current = { y: event.clientY, norm }
    event.currentTarget.setPointerCapture(event.pointerId)
    setDragging(true)
    event.preventDefault()
  }
  const onPointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!gesture.current || disabled) return
    const travel = event.shiftKey ? TRAVEL_PX * 5 : TRAVEL_PX
    onChange(
      fromNorm(gesture.current.norm + (gesture.current.y - event.clientY) / travel),
    )
  }
  const onPointerUp = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!gesture.current) return
    gesture.current = null
    event.currentTarget.releasePointerCapture(event.pointerId)
    setDragging(false)
  }

  return (
    <div className={`knob ${size}${disabled ? ' is-disabled' : ''}`}>
      <div
        className={`cap${dragging ? ' is-dragging' : ''}`}
        role="slider"
        aria-label={label}
        aria-valuemin={min}
        aria-valuemax={max}
        aria-valuenow={value}
        aria-valuetext={
          disabled && disabledReadout
            ? disabledReadout
            : `${display(value)} ${unit}`
        }
        aria-disabled={disabled}
        tabIndex={disabled ? -1 : 0}
        title={
          disabled
            ? disabledReadout ?? `${label} programmed by Auto Rel`
            : `${label} — drag vertically, Shift for fine, double-click to reset`
        }
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onDoubleClick={() => !disabled && onChange(defaultValue)}
        onWheel={(event) => {
          if (disabled) return
          event.preventDefault()
          const delta = event.shiftKey ? step / 5 : step
          onChange(
            clamp(value + (event.deltaY < 0 ? delta : -delta), min, max),
          )
        }}
        onKeyDown={(event) => {
          if (disabled) return
          const delta = event.shiftKey ? step / 5 : step
          if (event.key === 'ArrowUp' || event.key === 'ArrowRight') {
            event.preventDefault()
            onChange(clamp(value + delta, min, max))
          } else if (event.key === 'ArrowDown' || event.key === 'ArrowLeft') {
            event.preventDefault()
            onChange(clamp(value - delta, min, max))
          } else if (event.key === 'Home') {
            event.preventDefault()
            onChange(defaultValue)
          }
        }}
      >
        <svg viewBox="0 0 100 100" aria-hidden="true">
          <defs>
            <radialGradient id={`${uid}-body`} cx="0.32" cy="0.28" r="0.78">
              <stop offset="0" stopColor="#d8dee8" />
              <stop offset="0.45" stopColor="#9aa4b4" />
              <stop offset="1" stopColor="#4a5360" />
            </radialGradient>
            <linearGradient id={`${uid}-rim`} x1="0" x2="0" y1="0" y2="1">
              <stop offset="0" stopColor="#eef2f8" />
              <stop offset="0.5" stopColor="#8a95a6" />
              <stop offset="1" stopColor="#3a4250" />
            </linearGradient>
          </defs>
          <circle className="rim" cx="50" cy="50" r="31" fill={`url(#${uid}-rim)`} />
          <g
            style={{
              transform: `rotate(${pointerAngle + 90}deg)`,
              transformOrigin: '50px 50px',
            }}
          >
            <circle className="body" cx="50" cy="50" r="26" fill={`url(#${uid}-body)`} />
            <path className="gloss" d="M 34 38 A 20 20 0 0 1 66 34" />
            <rect className="pointer" x="48.7" y="24" width="2.6" height="16" rx="1.2" />
          </g>
        </svg>
      </div>
      <div className="plate">
        <div className="name">{label}</div>
        {editing && !disabled ? (
          <input
            ref={inputRef}
            className="input"
            value={draft}
            aria-label={`${label} value`}
            onChange={(event) => setDraft(event.target.value)}
            onBlur={() => {
              const parsed = Number(draft.replace(/[^\d.+-]/g, ''))
              if (Number.isFinite(parsed)) onChange(clamp(parsed, min, max))
              setEditing(false)
            }}
            onKeyDown={(event) => {
              if (event.key === 'Enter') (event.target as HTMLInputElement).blur()
              if (event.key === 'Escape') setEditing(false)
            }}
          />
        ) : (
          <button
            type="button"
            className="readout"
            disabled={disabled}
            title={disabled ? disabledReadout : 'Click to type a value'}
            onClick={() => {
              if (disabled) return
              setDraft(String(Number(value.toFixed(2))))
              setEditing(true)
            }}
          >
            {disabled && disabledReadout ? (
              disabledReadout
            ) : (
              <>
                {display(value)}
                <span className="unit">{unit}</span>
              </>
            )}
          </button>
        )}
      </div>
    </div>
  )
}

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

function App() {
  const [params, setParams] = useState<ZcompParams>(defaults)
  const [connected, setConnected] = useState(false)
  const [meterLive, setMeterLive] = useState(false)
  const [meterMode, setMeterMode] = useState<MeterMode>('reduction')
  const metersRef = useRef<MeterFrame | null>(null)
  const [preset, setPreset] = useState<number | null>(0)
  const [skinPulse, setSkinPulse] = useState(false)
  const prevModel = useRef(params.model)

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
            setMeterLive(false)
          }
        },
        (frame) => {
          metersRef.current = frame
          setMeterLive((was) => was || true)
        },
      ),
    [],
  )

  useEffect(() => {
    if (prevModel.current === params.model) return
    prevModel.current = params.model
    setSkinPulse(true)
    const timer = window.setTimeout(() => setSkinPulse(false), 420)
    return () => window.clearTimeout(timer)
  }, [params.model])

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
  const circuit = MODEL_CIRCUIT[params.model]
  const metersActive = connected && meterLive

  return (
    <div
      className={`unit${skinPulse ? ' is-skin-pulse' : ''}`}
      data-model={params.model}
      style={
        {
          '--drive': String(clamp(params.color / 100, 0, 1)),
        } as CSSProperties
      }
    >
      <div className={`panel${params.power ? '' : ' is-off'}`}>
        <header className="top">
          <div className="identity">
            <div className="wordmark">
              <span className="z">Z</span>
              <div>
                <strong>Z-Comp</strong>
                <em>{MODEL_LABEL[params.model]} circuit</em>
              </div>
            </div>
            <PresetControl
              preset={preset}
              names={FACTORY_PRESETS.map((entry) => entry.name)}
              onChange={loadPreset}
              onPrevious={() => loadPreset((preset ?? 0) - 1)}
              onNext={() => loadPreset((preset ?? -1) + 1)}
            />
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

          <div className="side">
            <div className="meter-switch" role="group" aria-label="Meter mode">
              <button
                type="button"
                className={meterMode === 'reduction' ? 'is-on' : undefined}
                onClick={() => setMeterMode('reduction')}
              >
                GR
              </button>
              <button
                type="button"
                className={meterMode === 'output' ? 'is-on' : undefined}
                onClick={() => setMeterMode('output')}
              >
                +4
              </button>
            </div>
            <button
              type="button"
              className={`power${params.power ? ' is-on' : ''}`}
              role="switch"
              aria-checked={params.power}
              aria-label="Power"
              onClick={() => change('power', !params.power, params.power ? 0 : 1)}
            >
              <span className="lamp" />
              <span className="legend">Power</span>
            </button>
            <span
              className={`link${connected ? ' is-live' : ''}`}
              title={connected ? 'Host linked' : 'Preview'}
            />
          </div>
        </header>

        <section className="models" aria-label="Compressor model">
          <span className="bank-label">Model</span>
          <div className="bank" role="radiogroup" aria-label="Circuit model">
            {MODELS.map((model) => (
              <button
                key={model}
                type="button"
                className={params.model === model ? 'is-on' : undefined}
                role="radio"
                aria-checked={params.model === model}
                onClick={() => change('model', model, MODEL_WIRE[model])}
              >
                {MODEL_LABEL[model]}
              </button>
            ))}
          </div>
          <button
            type="button"
            className={`auto${params.autoRelease ? ' is-on' : ''}`}
            aria-pressed={params.autoRelease}
            title={
              params.model === 'ssl'
                ? 'Program-dependent release (SSL path in DSP)'
                : 'Auto release flag (SSL circuit uses it)'
            }
            onClick={() =>
              change('autoRelease', !params.autoRelease, params.autoRelease ? 0 : 1)
            }
          >
            Auto Rel
          </button>
        </section>

        <p className="circuit" aria-live="polite">
          <strong>{circuit.title}</strong>
          <span>{circuit.topology}</span>
        </p>

        <section className="knobs" aria-label="Main controls">
          <AluminumKnob
            spec={PARAM_SPECS.thresholdDb}
            value={params.thresholdDb}
            display={formatDb}
            onChange={(value) => change('thresholdDb', value)}
          />
          <AluminumKnob
            spec={PARAM_SPECS.ratio}
            value={params.ratio}
            display={formatRatio}
            onChange={(value) => change('ratio', value)}
          />
          <AluminumKnob
            spec={PARAM_SPECS.attackMs}
            value={params.attackMs}
            display={formatMs}
            onChange={(value) => change('attackMs', value)}
          />
          <AluminumKnob
            spec={PARAM_SPECS.releaseMs}
            value={params.releaseMs}
            display={formatMs}
            onChange={(value) => change('releaseMs', value)}
            disabled={programmedRelease}
            disabledReadout="AUTO"
          />
          <AluminumKnob
            spec={PARAM_SPECS.makeupDb}
            value={params.makeupDb}
            display={formatDb}
            onChange={(value) => change('makeupDb', value)}
          />
        </section>

        <section className="extras" aria-label="Extras">
          <AluminumKnob
            size="sm"
            spec={PARAM_SPECS.kneeDb}
            value={params.kneeDb}
            display={formatDb}
            onChange={(value) => change('kneeDb', value)}
          />
          <AluminumKnob
            size="sm"
            spec={PARAM_SPECS.mix}
            value={params.mix}
            display={formatPct}
            onChange={(value) => change('mix', value)}
          />
          <AluminumKnob
            size="sm"
            spec={PARAM_SPECS.sidechainHpfHz}
            value={params.sidechainHpfHz}
            display={formatHz}
            onChange={(value) => change('sidechainHpfHz', value)}
          />
          <AluminumKnob
            size="sm"
            spec={PARAM_SPECS.stereoLink}
            value={params.stereoLink}
            display={formatPct}
            onChange={(value) => change('stereoLink', value)}
          />
          <AluminumKnob
            size="sm"
            spec={PARAM_SPECS.color}
            value={params.color}
            display={formatPct}
            onChange={(value) => change('color', value)}
          />
        </section>
      </div>
    </div>
  )
}

export default App
