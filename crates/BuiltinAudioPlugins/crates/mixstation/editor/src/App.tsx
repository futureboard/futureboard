import { useEffect, useMemo, useRef, useState } from 'react'
import { AnimatePresence, Reorder, motion } from 'motion/react'
import { animate } from 'animejs'
import {
  CaretDownIcon,
  CaretLeftIcon,
  CaretRightIcon,
  PlusIcon,
} from '@phosphor-icons/react'
import {
  connectBridge,
  defaults,
  postGlobalCommand,
  postParam,
  type MeterFrame,
  type MixStationParams,
} from './bridge'
import { BypassSwitch, IconButton } from './Controls'
import { Knob } from './Knob'
import { Meters } from './Meters'
import { ModuleRow } from './ModuleRow'
import { Popover } from './Popover'
import { PresetMenu } from './PresetMenu'
import {
  RACK_MODULES,
  SLOT_IDS,
  moduleByCode,
  slotCodes,
  type BooleanParamId,
  type RackModule,
} from './modules'
import { PARAM_SPECS, sanitizeParams, type NumericParamId } from './params'
import { FACTORY_PRESETS, matchingPresetIndex, postAllParams } from './presets'

export default function App() {
  const [params, setParams] = useState<MixStationParams>(defaults)
  const [connected, setConnected] = useState(false)
  const [meterLive, setMeterLive] = useState(false)
  const [presetIndex, setPresetIndex] = useState<number | null>(0)
  const [collapsed, setCollapsed] = useState<ReadonlySet<number>>(new Set())
  const [presetOpen, setPresetOpen] = useState(false)
  const [pickerOpen, setPickerOpen] = useState(false)
  const metersRef = useRef<MeterFrame | null>(null)
  const presetAnchor = useRef<HTMLButtonElement | null>(null)
  const pickerAnchor = useRef<HTMLButtonElement | null>(null)
  const presetNameRef = useRef<HTMLSpanElement | null>(null)

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.metaKey || e.altKey || e.repeat) return
      if (e.key !== ' ' && e.code !== 'Space') return
      const target = e.target as HTMLElement | null
      if (
        target &&
        (target.tagName === 'INPUT' ||
          target.tagName === 'TEXTAREA' ||
          target.tagName === 'SELECT' ||
          target.isContentEditable)
      ) {
        return
      }
      // DAW transport owns bare Space on the editor surface. Forward when the
      // native claim path misses an OSR/focus edge case.
      e.preventDefault()
      postGlobalCommand('transport:play-pause')
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [])

  useEffect(
    () =>
      connectBridge(
        (incoming) => {
          const clean = sanitizeParams(incoming)
          setParams(clean)
          setPresetIndex(matchingPresetIndex(clean))
          setPresetOpen(false)
          setPickerOpen(false)
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

  /** Rack edits touch several parameters at once; post them as one batch. */
  const commitRack = (next: MixStationParams) => {
    const clean = sanitizeParams(next)
    setParams(clean)
    setPresetIndex(null)
    for (const id of SLOT_IDS) postParam(id, clean[id])
    for (const module of RACK_MODULES) {
      postParam(module.enabledId, clean[module.enabledId] ? 1 : 0)
    }
  }

  const codes = slotCodes(params)
  const loaded = useMemo(
    () => codes.filter((code) => code !== 0),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [codes.join(',')],
  )
  const available = RACK_MODULES.filter((item) => !codes.includes(item.code))

  /** Write an ordered list of module codes back into the six slot parameters. */
  const writeOrder = (order: readonly number[], extra: Partial<MixStationParams> = {}) => {
    const next: MixStationParams = { ...params, ...extra }
    SLOT_IDS.forEach((id, index) => {
      next[id] = order[index] ?? 0
    })
    commitRack(next)
  }

  const addModule = (module: RackModule) => {
    setPickerOpen(false)
    if (codes.includes(module.code)) return
    writeOrder([...loaded, module.code], { [module.enabledId]: true })
  }

  const removeModule = (module: RackModule) => {
    writeOrder(
      loaded.filter((code) => code !== module.code),
      { [module.enabledId]: false },
    )
  }

  const moveModule = (code: number, delta: -1 | 1) => {
    const from = loaded.indexOf(code)
    const to = from + delta
    if (from < 0 || to < 0 || to >= loaded.length) return
    const order = [...loaded]
    ;[order[from], order[to]] = [order[to], order[from]]
    writeOrder(order)
  }

  /** Restore one module's parameters to the Rust-authored defaults. */
  const resetModule = (module: RackModule) => {
    const next = { ...params }
    for (const id of module.knobs) next[id] = defaults[id]
    setParams(sanitizeParams(next))
    setPresetIndex(null)
    for (const id of module.knobs) postParam(id, defaults[id])
  }

  const loadPreset = (index: number) => {
    const wrapped =
      ((index % FACTORY_PRESETS.length) + FACTORY_PRESETS.length) % FACTORY_PRESETS.length
    const next = sanitizeParams({ ...FACTORY_PRESETS[wrapped]!.params })
    setParams(next)
    setPresetIndex(wrapped)
    setPickerOpen(false)
    postAllParams(next)
  }

  const stepPreset = (delta: -1 | 1) => {
    loadPreset((presetIndex ?? (delta > 0 ? -1 : 0)) + delta)
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

  const toggleCollapsed = (code: number) =>
    setCollapsed((current) => {
      const next = new Set(current)
      if (!next.delete(code)) next.add(code)
      return next
    })

  const presetLabel =
    presetIndex === null ? 'Modified' : (FACTORY_PRESETS[presetIndex]?.name ?? 'Modified')

  return (
    <main className="flex h-full w-full flex-col overflow-hidden bg-workspace">
      <header className="flex h-12 shrink-0 items-center gap-4 border-b border-hairline bg-panel px-4">
        <div className="flex min-w-0 items-center gap-2.5">
          <h1 className="text-[15px] font-bold tracking-[-0.01em]">MixStation</h1>
          <span
            className="flex items-center gap-1.5 rounded border border-hairline-hi px-2 py-0.5 text-[10px] font-medium text-ink-muted"
            title={
              connected
                ? 'Bound to a plug-in instance'
                : 'No plug-in instance bound — controls show defaults'
            }
          >
            {/* Slow pulse only while a real instance is bound — the animation
                is the connection state, not decoration. */}
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
                  currentIndex={presetIndex}
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

        <div className="flex items-center gap-3">
          <span className="readout text-ink-muted">{loaded.length} / 6</span>
          <BypassSwitch
            on={params.power}
            label={params.power ? 'MixStation enabled' : 'MixStation bypassed'}
            onToggle={() => changeBoolean('power', !params.power)}
          />
        </div>
      </header>

      <div className="flex min-h-0 flex-1 flex-col overflow-auto bg-rack px-3 pb-4">
        <div className="mx-auto flex w-full max-w-[1280px] min-w-[900px] flex-col">
          <div className="flex h-9 shrink-0 items-center justify-between">
            <h2 className="label-cap">Signal path</h2>
            <span className="text-[10px] text-ink-dim">In → stages → Out</span>
          </div>

          {loaded.length === 0 ? (
            <motion.p
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              className="rounded-md border border-dashed border-hairline-hi px-4 py-8 text-center text-[12px] text-ink-dim"
            >
              The rack is empty. Add a module to start building the chain.
            </motion.p>
          ) : (
            <Reorder.Group
              axis="y"
              values={loaded}
              onReorder={(order) => writeOrder(order)}
              className="flex flex-col gap-1.5"
            >
              <AnimatePresence initial={false}>
                {loaded.map((code, index) => {
                  const module = moduleByCode(code)
                  if (!module) return null
                  return (
                    <ModuleRow
                      key={code}
                      module={module}
                      index={index}
                      params={params}
                      powered={params.power}
                      collapsed={collapsed.has(code)}
                      onToggleCollapsed={() => toggleCollapsed(code)}
                      onNumber={changeNumber}
                      onBypass={(value) => changeBoolean(module.enabledId, value)}
                      onRemove={() => removeModule(module)}
                      onReset={() => resetModule(module)}
                      onMove={(delta) => moveModule(code, delta)}
                    />
                  )
                })}
              </AnimatePresence>
            </Reorder.Group>
          )}

          <div className="relative mt-1.5">
            <button
              ref={pickerAnchor}
              type="button"
              aria-haspopup="menu"
              aria-expanded={pickerOpen}
              disabled={available.length === 0}
              onClick={() => setPickerOpen((open) => !open)}
              className="flex h-14 w-full cursor-pointer items-center gap-3 rounded-md border border-dashed border-hairline-hi bg-slot/50 px-3 text-left transition-colors duration-200 hover:border-signal/60 hover:bg-slot disabled:cursor-not-allowed disabled:opacity-45 disabled:hover:border-hairline-hi"
            >
              <span className="w-8 shrink-0 text-center">
                <span className="readout text-ink-dim">{loaded.length + 1}</span>
              </span>
              <span className="grid h-8 w-8 shrink-0 place-items-center rounded-full border border-hairline-hi text-ink-muted">
                <PlusIcon size={15} weight="bold" />
              </span>
              <span className="flex flex-col">
                <span className="text-[12px] font-medium text-ink">Add module</span>
                <span className="text-[10px] text-ink-dim">
                  {available.length === 0
                    ? 'All six modules are loaded'
                    : `${available.length} available`}
                </span>
              </span>
            </button>

            <AnimatePresence>
              {pickerOpen && available.length > 0 && (
                <Popover
                  anchorRef={pickerAnchor}
                  onClose={() => setPickerOpen(false)}
                  align="start"
                  width={340}
                  className="p-1.5"
                >
                  <p className="label-cap px-2 pt-1 pb-2">Append to chain</p>
                  <div role="menu" className="flex flex-col">
                    {available.map((module, position) => (
                      <motion.button
                        key={module.code}
                        type="button"
                        role="menuitem"
                        onClick={() => addModule(module)}
                        initial={{ opacity: 0, y: 6 }}
                        animate={{ opacity: 1, y: 0 }}
                        transition={{
                          delay: position * 0.03,
                          duration: 0.2,
                          ease: [0.16, 1, 0.3, 1],
                        }}
                        className="flex cursor-pointer items-start gap-2.5 rounded px-2.5 py-2 text-left transition-colors duration-150 hover:bg-white/6"
                      >
                        <span
                          aria-hidden
                          className="mt-1.5 h-1.5 w-1.5 shrink-0 rounded-full"
                          style={{ background: module.accent }}
                        />
                        <span className="min-w-0">
                          <span className="block text-[12px] font-semibold text-ink">
                            {module.name}
                          </span>
                          <span className="block truncate text-[10px] text-ink-dim">
                            {module.hint}
                          </span>
                        </span>
                      </motion.button>
                    ))}
                  </div>
                </Popover>
              )}
            </AnimatePresence>
          </div>

          <footer className="plane mt-3 flex shrink-0 items-center gap-6 rounded-md border border-hairline bg-slot px-4 py-3">
            <div className="flex w-[120px] shrink-0 items-center gap-2">
              <span className="label-cap">In</span>
              <Knob
                spec={PARAM_SPECS.inputTrimDb}
                value={params.inputTrimDb}
                bipolar
                disabled={!params.power}
                onChange={(value) => changeNumber('inputTrimDb', value)}
              />
            </div>
            <div className="flex flex-1 justify-center">
              <Meters metersRef={metersRef} live={connected && meterLive} />
            </div>
            <div className="flex w-[120px] shrink-0 items-center justify-end gap-2">
              <Knob
                spec={PARAM_SPECS.outputTrimDb}
                value={params.outputTrimDb}
                bipolar
                disabled={!params.power}
                onChange={(value) => changeNumber('outputTrimDb', value)}
              />
              <span className="label-cap">Out</span>
            </div>
          </footer>
        </div>
      </div>
    </main>
  )
}
