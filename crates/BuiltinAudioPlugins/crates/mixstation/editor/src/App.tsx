import {
  useEffect,
  useRef,
  useState,
  type DragEvent,
  type ReactNode,
} from 'react'
import {
  connectBridge,
  defaults,
  postParam,
  type MeterFrame,
  type MixStationParams,
} from './bridge'
import { Knob } from './Knob'
import { Meters } from './Meters'
import { PARAM_SPECS, sanitizeParams, type NumericParamId } from './params'
import {
  FACTORY_PRESETS,
  matchingPresetIndex,
  postAllParams,
} from './presets'

type BooleanParamId =
  | 'power'
  | 'filtersEnabled'
  | 'eqEnabled'
  | 'compEnabled'
  | 'satEnabled'
  | 'widthEnabled'
  | 'limiterEnabled'

type RackModuleId = Exclude<BooleanParamId, 'power'>

type RackModule = {
  code: number
  id: RackModuleId
  name: string
  shortName: string
  category: string
  className: string
}

const RACK_MODULES: readonly RackModule[] = [
  {
    code: 1,
    id: 'filtersEnabled',
    name: 'Precision Filters',
    shortName: 'Filters',
    category: 'Utility',
    className: 'filters-device',
  },
  {
    code: 2,
    id: 'eqEnabled',
    name: 'Console EQ',
    shortName: '4-Band EQ',
    category: 'Equalizer',
    className: 'eq-device',
  },
  {
    code: 3,
    id: 'compEnabled',
    name: 'Rack Compressor',
    shortName: 'Compressor',
    category: 'Dynamics',
    className: 'comp-device',
  },
  {
    code: 4,
    id: 'satEnabled',
    name: 'Color Drive',
    shortName: 'Saturation',
    category: 'Color',
    className: 'sat-device',
  },
  {
    code: 5,
    id: 'widthEnabled',
    name: 'Stereo Field',
    shortName: 'Stereo',
    category: 'Imaging',
    className: 'width-device',
  },
  {
    code: 6,
    id: 'limiterEnabled',
    name: 'Safety Limiter',
    shortName: 'Limiter',
    category: 'Dynamics',
    className: 'limiter-device',
  },
] as const

const SLOT_IDS = [
  'slot1Module',
  'slot2Module',
  'slot3Module',
  'slot4Module',
  'slot5Module',
  'slot6Module',
] as const satisfies readonly NumericParamId[]

function Device({
  title,
  enabled,
  onToggle,
  onRemove,
  children,
  className = '',
}: {
  title: string
  enabled: boolean
  onToggle: () => void
  onRemove: () => void
  children: ReactNode
  className?: string
}) {
  return (
    <section
      className={`rack-device ${className}${enabled ? '' : ' bypassed'}`}
      aria-label={title}
    >
      <header className="device-head">
        <button
          type="button"
          className={`device-power${enabled ? ' enabled' : ''}`}
          role="switch"
          aria-checked={enabled}
          aria-label={`${title} ${enabled ? 'enabled' : 'bypassed'}`}
          onClick={onToggle}
        >
          <span />
        </button>
        <h2>{title}</h2>
        <button
          type="button"
          className="device-remove"
          aria-label={`Remove ${title}`}
          onClick={onRemove}
        >
          ×
        </button>
      </header>
      <div className="device-face">{children}</div>
      <footer className="device-foot">
        <span>Fixed stage</span>
        <i />
        <strong>In</strong>
      </footer>
    </section>
  )
}

export default function App() {
  const [params, setParams] = useState<MixStationParams>(defaults)
  const [connected, setConnected] = useState(false)
  const [meterLive, setMeterLive] = useState(false)
  const [presetIndex, setPresetIndex] = useState<number | null>(0)
  const metersRef = useRef<MeterFrame | null>(null)

  useEffect(
    () =>
      connectBridge(
        (incoming) => {
          const clean = sanitizeParams(incoming)
          setParams(clean)
          setPresetIndex(matchingPresetIndex(clean))
          metersRef.current = null
          setMeterLive(false)
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
          setMeterLive(true)
        },
      ),
    [],
  )

  const changeNumber = (id: NumericParamId, value: number) => {
    setParams((current) => sanitizeParams({ ...current, [id]: value }))
    setPresetIndex(null)
    postParam(id, value)
  }

  const changeBoolean = (id: BooleanParamId, value: boolean) => {
    setParams((current) => ({ ...current, [id]: value }))
    setPresetIndex(null)
    postParam(id, value ? 1 : 0)
  }

  const knob = (id: NumericParamId, enabled = true) => (
    <Knob
      key={id}
      spec={PARAM_SPECS[id]}
      value={params[id]}
      disabled={!enabled || !params.power}
      onChange={(value) => changeNumber(id, value)}
    />
  )

  const loadPreset = (index: number) => {
    const wrapped =
      ((index % FACTORY_PRESETS.length) + FACTORY_PRESETS.length) %
      FACTORY_PRESETS.length
    const next = sanitizeParams({ ...FACTORY_PRESETS[wrapped]!.params })
    setParams(next)
    setPresetIndex(wrapped)
    postAllParams(next)
  }

  const slotValues = SLOT_IDS.map((id) => Math.round(params[id]))

  const commitRack = (next: MixStationParams) => {
    const clean = sanitizeParams(next)
    setParams(clean)
    setPresetIndex(null)
    for (const id of SLOT_IDS) postParam(id, clean[id])
    for (const module of RACK_MODULES) {
      postParam(module.id, clean[module.id] ? 1 : 0)
    }
  }

  const installModule = (module: RackModule) => {
    if (slotValues.includes(module.code)) return
    const emptySlot = slotValues.indexOf(0)
    if (emptySlot < 0) return
    commitRack({
      ...params,
      [SLOT_IDS[emptySlot]!]: module.code,
      [module.id]: true,
    })
  }

  const removeModule = (slotIndex: number, module: RackModule) => {
    commitRack({
      ...params,
      [SLOT_IDS[slotIndex]!]: 0,
      [module.id]: false,
    })
  }

  const beginDrag = (
    event: DragEvent<HTMLElement>,
    module: RackModule,
    sourceSlot: number | null,
  ) => {
    event.dataTransfer.effectAllowed = sourceSlot === null ? 'copy' : 'move'
    event.dataTransfer.setData(
      'application/x-mixstation-module',
      JSON.stringify({ code: module.code, sourceSlot }),
    )
  }

  const dropIntoSlot = (event: DragEvent<HTMLElement>, targetSlot: number) => {
    event.preventDefault()
    let payload: { code: number; sourceSlot: number | null }
    try {
      payload = JSON.parse(
        event.dataTransfer.getData('application/x-mixstation-module'),
      ) as typeof payload
    } catch {
      return
    }
    const module = RACK_MODULES.find((item) => item.code === payload.code)
    if (!module || payload.sourceSlot === targetSlot) return

    const next = { ...params }
    const displacedCode = slotValues[targetSlot] ?? 0
    if (payload.sourceSlot !== null) {
      next[SLOT_IDS[payload.sourceSlot]!] = displacedCode
    } else if (displacedCode !== 0) {
      const displaced = RACK_MODULES.find((item) => item.code === displacedCode)
      if (displaced) next[displaced.id] = false
    }
    next[SLOT_IDS[targetSlot]!] = module.code
    next[module.id] = true
    commitRack(next)
  }

  const renderDevice = (module: RackModule) => {
    switch (module.id) {
      case 'filtersEnabled':
        return (
          <>
            <div className="device-mark">HP / LP</div>
            {knob('hpfHz', params.filtersEnabled)}
            {knob('lpfHz', params.filtersEnabled)}
          </>
        )
      case 'eqEnabled':
        return (
          <>
            {knob('lowGainDb', params.eqEnabled)}
            {knob('lowMidFreqHz', params.eqEnabled)}
            {knob('lowMidGainDb', params.eqEnabled)}
            {knob('highMidFreqHz', params.eqEnabled)}
            {knob('highMidGainDb', params.eqEnabled)}
            {knob('highGainDb', params.eqEnabled)}
          </>
        )
      case 'compEnabled':
        return (
          <>
            <div className="device-mark">Dynamics</div>
            {knob('compThresholdDb', params.compEnabled)}
            {knob('compRatio', params.compEnabled)}
            {knob('compAttackMs', params.compEnabled)}
            {knob('compReleaseMs', params.compEnabled)}
            {knob('compMakeupDb', params.compEnabled)}
          </>
        )
      case 'satEnabled':
        return (
          <>
            <div className="device-mark">COLOR</div>
            {knob('satDrivePct', params.satEnabled)}
            {knob('satCharacterPct', params.satEnabled)}
          </>
        )
      case 'widthEnabled':
        return (
          <>
            <div className="stereo-glyph" aria-hidden="true">
              L <i /> R
            </div>
            {knob('widthPct', params.widthEnabled)}
          </>
        )
      case 'limiterEnabled':
        return (
          <>
            <div className="device-mark">Zero latency</div>
            {knob('limiterCeilingDb', params.limiterEnabled)}
            {knob('limiterReleaseMs', params.limiterEnabled)}
          </>
        )
    }
  }

  return (
    <main className={`rack${params.power ? '' : ' powered-off'}`}>
      <header className="topbar">
        <div className="identity">
          <span className="rack-logo">M</span>
          <h1>MixStation</h1>
          <span className="descriptor">Virtual Mix Rack</span>
        </div>

        <div className="preset-control" aria-label="Factory preset">
          <button
            type="button"
            aria-label="Previous preset"
            onClick={() => loadPreset((presetIndex ?? 0) - 1)}
          >
            ‹
          </button>
          <label>
            <span>Preset</span>
            <select
              value={presetIndex ?? ''}
              onChange={(event) => loadPreset(Number(event.target.value))}
            >
              {presetIndex === null ? <option value="">Modified</option> : null}
              {FACTORY_PRESETS.map((preset, index) => (
                <option key={preset.name} value={index}>
                  {preset.name}
                </option>
              ))}
            </select>
          </label>
          <button
            type="button"
            aria-label="Next preset"
            onClick={() => loadPreset((presetIndex ?? -1) + 1)}
          >
            ›
          </button>
        </div>

        <div className="host-state" aria-live="polite">
          <span className={connected ? 'linked' : ''} />
          <div>
            <small>Host</small>
            <strong>{connected ? 'Linked' : 'Standby'}</strong>
          </div>
        </div>

        <button
          type="button"
          className={`power${params.power ? ' enabled' : ''}`}
          role="switch"
          aria-checked={params.power}
          aria-label={params.power ? 'MixStation enabled' : 'MixStation bypassed'}
          onClick={() => changeBoolean('power', !params.power)}
        >
          <span />
          {params.power ? 'Active' : 'Bypass'}
        </button>
      </header>

      <div className="rack-workspace">
        <aside className="module-browser" aria-label="Rack modules">
          <div className="browser-tabs">
            <button type="button" className="active">
              Modules
            </button>
            <button type="button" disabled>
              Macro
            </button>
          </div>
          <label className="module-search">
            <span>⌕</span>
            <input aria-label="Search modules" placeholder="Search modules…" />
          </label>
          <div className="module-list">
            {RACK_MODULES.map((module) => (
              <button
                type="button"
                key={module.id}
                className={slotValues.includes(module.code) ? 'installed' : ''}
                onClick={() => installModule(module)}
                disabled={slotValues.includes(module.code)}
                draggable={!slotValues.includes(module.code)}
                onDragStart={(event) => beginDrag(event, module, null)}
              >
                <span className={`module-thumb ${module.className}`}>M</span>
                <span>
                  <strong>{module.name}</strong>
                  <small>{module.category} · drag to any slot</small>
                </span>
                <i>{slotValues.includes(module.code) ? 'In rack' : '+'}</i>
              </button>
            ))}
          </div>
        </aside>

        <section className="rack-stage" aria-label="MixStation signal chain">
          <div className="device-chain">
            {SLOT_IDS.map((slotId, index) => {
              const module = RACK_MODULES.find(
                (item) => item.code === Math.round(params[slotId]),
              )
              return (
              <div
                className={`rack-slot${module ? ' loaded' : ''}`}
                key={slotId}
                draggable={Boolean(module)}
                onDragStart={(event) => {
                  if (module) beginDrag(event, module, index)
                }}
                onDragOver={(event) => {
                  event.preventDefault()
                  event.dataTransfer.dropEffect = module ? 'move' : 'copy'
                }}
                onDrop={(event) => dropIntoSlot(event, index)}
              >
                {module ? (
                  <Device
                    title={module.shortName}
                    className={module.className}
                    enabled={params[module.id]}
                    onToggle={() =>
                      changeBoolean(module.id, !params[module.id])
                    }
                    onRemove={() => removeModule(index, module)}
                  >
                    {renderDevice(module)}
                  </Device>
                ) : (
                  <div
                    className="empty-slot"
                    aria-label={`Empty rack slot ${index + 1}; drop any module here`}
                  >
                    <span>+</span>
                    <strong>Empty slot</strong>
                    <small>{index + 1} · drop any module</small>
                  </div>
                )}
              </div>
              )
            })}
          </div>
        </section>
      </div>

      <footer className="meter-dock">
        <div className="trim-control">
          <span>Input trim</span>
          {knob('inputTrimDb')}
        </div>
        <div className="signal-caption">
          <span>Input</span>
          <i />
          <span>{slotValues.filter((module) => module !== 0).length} modules</span>
          <i />
          <span>Output</span>
        </div>
        <Meters metersRef={metersRef} live={connected && meterLive} />
        <div className="trim-control">
          <span>Output trim</span>
          {knob('outputTrimDb')}
        </div>
      </footer>
    </main>
  )
}
