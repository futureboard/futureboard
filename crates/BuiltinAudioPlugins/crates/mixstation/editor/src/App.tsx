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
  hint: string
}

const RACK_MODULES: readonly RackModule[] = [
  { code: 1, id: 'filtersEnabled', name: 'Filters', hint: 'High / low cut' },
  { code: 2, id: 'eqEnabled', name: 'EQ', hint: '4-band' },
  { code: 3, id: 'compEnabled', name: 'Compressor', hint: 'Dynamics' },
  { code: 4, id: 'satEnabled', name: 'Drive', hint: 'Saturation' },
  { code: 5, id: 'widthEnabled', name: 'Width', hint: 'Stereo image' },
  { code: 6, id: 'limiterEnabled', name: 'Limiter', hint: 'Ceiling' },
] as const

const SLOT_IDS = [
  'slot1Module',
  'slot2Module',
  'slot3Module',
  'slot4Module',
  'slot5Module',
  'slot6Module',
] as const satisfies readonly NumericParamId[]

const DRAG_TYPE = 'application/x-mixstation-module'

export default function App() {
  const [params, setParams] = useState<MixStationParams>(defaults)
  const [connected, setConnected] = useState(false)
  const [meterLive, setMeterLive] = useState(false)
  const [presetIndex, setPresetIndex] = useState<number | null>(0)
  const [dragOverSlot, setDragOverSlot] = useState<number | null>(null)
  const [dragging, setDragging] = useState(false)
  const [pickerSlot, setPickerSlot] = useState<number | null>(null)
  const metersRef = useRef<MeterFrame | null>(null)

  useEffect(
    () =>
      connectBridge(
        (incoming) => {
          const clean = sanitizeParams(incoming)
          setParams(clean)
          setPresetIndex(matchingPresetIndex(clean))
          setPickerSlot(null)
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
    setPickerSlot(null)
    postAllParams(next)
  }

  const slotValues = SLOT_IDS.map((id) => Math.round(params[id]))
  const loadedCount = slotValues.filter((code) => code !== 0).length

  const commitRack = (next: MixStationParams) => {
    const clean = sanitizeParams(next)
    setParams(clean)
    setPresetIndex(null)
    for (const id of SLOT_IDS) postParam(id, clean[id])
    for (const module of RACK_MODULES) {
      postParam(module.id, clean[module.id] ? 1 : 0)
    }
  }

  const installModule = (module: RackModule, slotIndex: number) => {
    setPickerSlot(null)
    if (slotValues.includes(module.code)) return
    commitRack({
      ...params,
      [SLOT_IDS[slotIndex]!]: module.code,
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

  const endDrag = () => {
    setDragging(false)
    setDragOverSlot(null)
  }

  const beginDrag = (
    event: DragEvent<HTMLElement>,
    module: RackModule,
    sourceSlot: number,
  ) => {
    event.dataTransfer.effectAllowed = 'move'
    event.dataTransfer.setData(
      DRAG_TYPE,
      JSON.stringify({ code: module.code, sourceSlot }),
    )
    setDragging(true)
    setPickerSlot(null)
  }

  /** Swap the dragged module with whatever occupies the target slot. */
  const dropIntoSlot = (event: DragEvent<HTMLElement>, targetSlot: number) => {
    event.preventDefault()
    endDrag()
    let payload: { code: number; sourceSlot: number }
    try {
      payload = JSON.parse(
        event.dataTransfer.getData(DRAG_TYPE),
      ) as typeof payload
    } catch {
      return
    }
    const module = RACK_MODULES.find((item) => item.code === payload.code)
    if (!module || payload.sourceSlot === targetSlot) return

    commitRack({
      ...params,
      [SLOT_IDS[payload.sourceSlot]!]: slotValues[targetSlot] ?? 0,
      [SLOT_IDS[targetSlot]!]: module.code,
    })
  }

  const renderDevice = (module: RackModule) => {
    const on = params[module.id]
    switch (module.id) {
      case 'filtersEnabled':
        return (
          <>
            {knob('hpfHz', on)}
            {knob('lpfHz', on)}
          </>
        )
      case 'eqEnabled':
        return (
          <>
            {knob('lowGainDb', on)}
            {knob('highGainDb', on)}
            {knob('lowMidFreqHz', on)}
            {knob('highMidFreqHz', on)}
            {knob('lowMidGainDb', on)}
            {knob('highMidGainDb', on)}
          </>
        )
      case 'compEnabled':
        return (
          <>
            {knob('compThresholdDb', on)}
            {knob('compRatio', on)}
            {knob('compAttackMs', on)}
            {knob('compReleaseMs', on)}
            {knob('compMakeupDb', on)}
          </>
        )
      case 'satEnabled':
        return (
          <>
            {knob('satDrivePct', on)}
            {knob('satCharacterPct', on)}
          </>
        )
      case 'widthEnabled':
        return <>{knob('widthPct', on)}</>
      case 'limiterEnabled':
        return (
          <>
            {knob('limiterCeilingDb', on)}
            {knob('limiterReleaseMs', on)}
          </>
        )
    }
  }

  const loadedSlot = (module: RackModule, index: number): ReactNode => {
    const on = params[module.id]
    return (
      <>
        <header
          className="slot-head"
          draggable
          onDragStart={(event) => beginDrag(event, module, index)}
          title="Drag to reorder"
        >
          <span className="slot-no">{index + 1}</span>
          <h2>{module.name}</h2>
          <button
            type="button"
            className={`slot-bypass${on ? ' on' : ''}`}
            role="switch"
            aria-checked={on}
            aria-label={`${module.name} ${on ? 'active' : 'bypassed'}`}
            onClick={() => changeBoolean(module.id, !on)}
          >
            <span className="lamp" />
            <span>{on ? 'On' : 'Off'}</span>
          </button>
          <button
            type="button"
            className="slot-x"
            aria-label={`Remove ${module.name}`}
            onClick={() => removeModule(index, module)}
          >
            ×
          </button>
        </header>
        <div className={`slot-body${on ? '' : ' is-off'}`}>
          {renderDevice(module)}
        </div>
      </>
    )
  }

  const vacantSlot = (index: number): ReactNode => {
    if (pickerSlot !== index) {
      return (
        <button
          type="button"
          className="slot-add"
          onClick={() => setPickerSlot(index)}
          aria-label={`Add a module at position ${index + 1}`}
        >
          <span className="slot-no">{index + 1}</span>
          <span className="slot-plus" aria-hidden="true">
            +
          </span>
          <span className="slot-add-label">Add</span>
        </button>
      )
    }
    const available = RACK_MODULES.filter(
      (item) => !slotValues.includes(item.code),
    )
    return (
      <div className="picker">
        <div className="picker-head">
          <span>Stage {index + 1}</span>
          <button
            type="button"
            aria-label="Close module picker"
            onClick={() => setPickerSlot(null)}
          >
            ×
          </button>
        </div>
        {available.length === 0 ? (
          <p className="picker-empty">Chain is full.</p>
        ) : (
          <ul className="picker-list">
            {available.map((item) => (
              <li key={item.id}>
                <button type="button" onClick={() => installModule(item, index)}>
                  <span className="picker-name">{item.name}</span>
                  <span className="picker-hint">{item.hint}</span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    )
  }

  return (
    <main
      className={`app${params.power ? '' : ' is-bypassed'}${dragging ? ' is-dragging' : ''}`}
      onDragEnd={endDrag}
    >
      <header className="top">
        <div className="brand">
          <strong>MixStation</strong>
          <span className={`host${connected ? ' live' : ''}`}>
            {connected ? 'Linked' : 'Standby'}
          </span>
        </div>

        <div className="preset" aria-label="Factory preset">
          <button
            type="button"
            aria-label="Previous preset"
            onClick={() => loadPreset((presetIndex ?? 0) - 1)}
          >
            ‹
          </button>
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
          <button
            type="button"
            aria-label="Next preset"
            onClick={() => loadPreset((presetIndex ?? -1) + 1)}
          >
            ›
          </button>
        </div>

        <div className="top-meta">
          <span className="count">
            <em>{loadedCount}</em>
            <span>/6</span>
          </span>
          <button
            type="button"
            className={`power${params.power ? ' on' : ''}`}
            role="switch"
            aria-checked={params.power}
            aria-label={
              params.power ? 'MixStation enabled' : 'MixStation bypassed'
            }
            onClick={() => changeBoolean('power', !params.power)}
          >
            <span className="lamp" />
            {params.power ? 'On' : 'Off'}
          </button>
        </div>
      </header>

      <div className="body">
        <div className="chain-label">
          <span>Signal path</span>
          <span className="path-hint">In → stages → Out</span>
        </div>

        <section className="chain" aria-label="Signal chain">
          {SLOT_IDS.map((slotId, index) => {
            const module = RACK_MODULES.find(
              (item) => item.code === Math.round(params[slotId]),
            )
            return (
              <div
                key={slotId}
                className={`slot${module ? ' loaded' : ' vacant'}${
                  dragOverSlot === index ? ' over' : ''
                }`}
                onDragOver={(event) => {
                  if (!dragging) return
                  event.preventDefault()
                  event.dataTransfer.dropEffect = 'move'
                  setDragOverSlot(index)
                }}
                onDragLeave={(event) => {
                  if (
                    !event.currentTarget.contains(event.relatedTarget as Node)
                  ) {
                    setDragOverSlot((current) =>
                      current === index ? null : current,
                    )
                  }
                }}
                onDrop={(event) => dropIntoSlot(event, index)}
              >
                {module ? loadedSlot(module, index) : vacantSlot(index)}
              </div>
            )
          })}
        </section>

        <footer className="floor">
          <div className="trim">
            <span className="trim-label">In</span>
            {knob('inputTrimDb')}
          </div>
          <div className="floor-meters">
            <Meters metersRef={metersRef} live={connected && meterLive} />
          </div>
          <div className="trim">
            <span className="trim-label">Out</span>
            {knob('outputTrimDb')}
          </div>
        </footer>
      </div>
    </main>
  )
}
