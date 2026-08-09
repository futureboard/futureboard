import { useRef, useState, type RefObject } from 'react'
import {
  AnimatePresence,
  Reorder,
  motion,
  useDragControls,
  useReducedMotion,
} from 'motion/react'
import { animate } from 'animejs'
import {
  ArrowCounterClockwiseIcon,
  CaretDownIcon,
  DotsSixVerticalIcon,
  DotsThreeVerticalIcon,
  TrashIcon,
} from '@phosphor-icons/react'
import { BypassSwitch, IconButton } from './Controls'
import { Knob } from './Knob'
import { StageMeter } from './Meters'
import { Popover } from './Popover'
import {
  CompressorCurve,
  EqCurve,
  FilterCurve,
  LimiterCurve,
  ReductionBar,
  SaturationCurve,
  WidthGraphic,
} from './Curves'
import { PARAM_SPECS, type NumericParamId } from './params'
import type { RackModule } from './modules'
import type { MeterFrame, MixStationParams, SpectrumFrame } from './bridge'

type Props = {
  module: RackModule
  index: number
  params: MixStationParams
  /** Whole-plugin power; every module control follows it. */
  powered: boolean
  collapsed: boolean
  onToggleCollapsed: () => void
  onNumber: (id: NumericParamId, value: number) => void
  onBypass: (value: boolean) => void
  onRemove: () => void
  onReset: () => void
  onMove: (delta: -1 | 1) => void
  spectrumRef: RefObject<SpectrumFrame | null>
  metersRef: RefObject<MeterFrame | null>
  spectrumLive: boolean
  metersLive: boolean
}

export function ModuleRow({
  module,
  index,
  params,
  powered,
  collapsed,
  onToggleCollapsed,
  onNumber,
  onBypass,
  onRemove,
  onReset,
  onMove,
  spectrumRef,
  metersRef,
  spectrumLive,
  metersLive,
}: Props) {
  const controls = useDragControls()
  const menuAnchor = useRef<HTMLDivElement | null>(null)
  const rowRef = useRef<HTMLDivElement | null>(null)
  const [menuOpen, setMenuOpen] = useState(false)
  const reducedMotion = useReducedMotion()

  const on = params[module.enabledId] && powered
  const accent = module.accent

  /**
   * Anime.js accent wash confirming the module just came back into the path.
   * Motion here reports cause and effect — it only fires on the off→on edge,
   * never on idle state.
   */
  const flashOn = () => {
    if (params[module.enabledId] || reducedMotion || !rowRef.current) return
    animate(rowRef.current, {
      backgroundColor: [
        { to: `color-mix(in srgb, ${accent} 14%, var(--color-slot))`, duration: 90 },
        { to: 'var(--color-slot)', duration: 400 },
      ],
      ease: 'outQuad',
    })
  }

  const knob = (id: NumericParamId, bipolar = false) => (
    <Knob
      key={id}
      spec={PARAM_SPECS[id]}
      value={params[id]}
      accent={accent}
      bipolar={bipolar}
      disabled={!on}
      onChange={(value) => onNumber(id, value)}
    />
  )

  return (
    <Reorder.Item
      value={module.code}
      dragListener={false}
      dragControls={controls}
      layout="position"
      /* Rows enter from below (deeper in the chain) and leave upward. */
      initial={{ opacity: 0, y: 10 }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: -6, transition: { duration: 0.14, ease: 'easeIn' } }}
      transition={{ type: 'spring', stiffness: 420, damping: 38 }}
      whileDrag={{
        scale: 1.006,
        zIndex: 20,
        boxShadow: '0 14px 34px rgb(0 0 0 / 0.5)',
      }}
      className="list-none"
    >
      <div
        ref={rowRef}
        className="plane relative flex h-[92px] items-stretch overflow-hidden rounded-md border border-hairline bg-slot"
        style={{ opacity: on ? 1 : 0.62, transition: 'opacity 160ms ease-out' }}
      >
        {/* Identity rule — a thin state marker, not an ornamental accent bar.
            It wipes in from the top so enabling a module reads as the stage
            rejoining the signal path. */}
        <span
          aria-hidden
          className="relative w-[3px] shrink-0 bg-hairline-hi"
        >
          <motion.span
            className="absolute inset-x-0 top-0 origin-top"
            style={{ background: accent }}
            initial={false}
            animate={{ height: on ? '100%' : '0%' }}
            transition={{ duration: reducedMotion ? 0 : 0.24, ease: [0.16, 1, 0.3, 1] }}
          />
        </span>

        <div className="flex w-8 shrink-0 flex-col items-center justify-center gap-1 py-2">
          <button
            type="button"
            aria-label={`Reorder ${module.name} — drag, or use the arrow keys`}
            title="Drag to reorder (↑ ↓ when focused)"
            onPointerDown={(event) => controls.start(event)}
            onKeyDown={(event) => {
              if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return
              event.preventDefault()
              onMove(event.key === 'ArrowUp' ? -1 : 1)
            }}
            className="grid h-7 w-7 cursor-grab place-items-center rounded text-ink-dim transition-colors duration-150 hover:bg-white/6 hover:text-ink active:cursor-grabbing"
          >
            <DotsSixVerticalIcon size={14} weight="bold" />
          </button>
          <span className="readout text-ink-dim">{index + 1}</span>
        </div>

        <div className="flex w-[186px] shrink-0 items-center gap-2 pr-3">
          <div className="min-w-0 flex-1">
            <h2
              className="truncate text-[11.5px] font-bold tracking-[0.09em] uppercase"
              title={module.hint}
            >
              {module.name}
            </h2>
            <p className="truncate text-[10px] text-ink-dim">{module.hint}</p>
          </div>
          <BypassSwitch
            on={params[module.enabledId]}
            accent={accent}
            disabled={!powered}
            label={`${module.name} ${params[module.enabledId] ? 'active' : 'bypassed'}`}
            onToggle={() => {
              flashOn()
              onBypass(!params[module.enabledId])
            }}
          />
          <IconButton
            label={collapsed ? `Expand ${module.name}` : `Collapse ${module.name}`}
            onClick={onToggleCollapsed}
          >
            <motion.span
              animate={{ rotate: collapsed ? -90 : 0 }}
              transition={{ duration: 0.16, ease: 'easeOut' }}
              className="grid place-items-center"
            >
              <CaretDownIcon size={13} weight="bold" />
            </motion.span>
          </IconButton>
        </div>

        {/* Crossfade for content replacement inside one container. */}
        <AnimatePresence initial={false} mode="wait">
          <motion.div
            key={collapsed ? 'collapsed' : 'expanded'}
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: reducedMotion ? 0 : 0.14, ease: 'easeOut' }}
            className="flex min-w-0 flex-1 items-center gap-3 border-l border-hairline/70 px-3"
          >
            {collapsed ? (
              <p className="truncate text-[11px] text-ink-dim">
                {module.knobs.length} parameter{module.knobs.length === 1 ? '' : 's'} hidden
              </p>
            ) : (
              <ModuleBody
                module={module}
                params={params}
                on={on}
                knob={knob}
                onNumber={onNumber}
                spectrumRef={spectrumRef}
                spectrumLive={spectrumLive}
                metersRef={metersRef}
                metersLive={metersLive}
              />
            )}
          </motion.div>
        </AnimatePresence>

        {/* Per-stage telemetry, this module's output trim, and its menu. */}
        <div className="flex shrink-0 items-center gap-2.5 border-l border-hairline/70 px-2.5">
          <StageMeter
            metersRef={metersRef}
            slot={index}
            live={metersLive && on}
            accent={accent}
          />
          <Knob
            spec={PARAM_SPECS[module.trimId]}
            value={params[module.trimId]}
            accent={accent}
            size={40}
            bipolar
            disabled={!on}
            onChange={(value) => onNumber(module.trimId, value)}
          />
          <div ref={menuAnchor}>
            <IconButton
              label={`${module.name} options`}
              active={menuOpen}
              onClick={() => setMenuOpen((open) => !open)}
            >
              <DotsThreeVerticalIcon size={16} weight="bold" />
            </IconButton>
          </div>
          <AnimatePresence>
            {menuOpen && (
              <Popover
                anchorRef={menuAnchor}
                onClose={() => setMenuOpen(false)}
                align="end"
                width={208}
              >
                <div role="menu" className="py-1">
                  <MenuItem
                    label="Reset module parameters"
                    icon={<ArrowCounterClockwiseIcon size={14} />}
                    onClick={() => {
                      onReset()
                      setMenuOpen(false)
                    }}
                  />
                  <div className="my-1 h-px bg-hairline" />
                  <MenuItem
                    label="Remove from chain"
                    icon={<TrashIcon size={14} />}
                    danger
                    onClick={() => {
                      onRemove()
                      setMenuOpen(false)
                    }}
                  />
                </div>
              </Popover>
            )}
          </AnimatePresence>
        </div>
      </div>
    </Reorder.Item>
  )
}

function MenuItem({
  label,
  icon,
  onClick,
  danger = false,
}: {
  label: string
  icon: React.ReactNode
  onClick: () => void
  danger?: boolean
}) {
  return (
    <button
      type="button"
      role="menuitem"
      onClick={onClick}
      className={`flex w-full cursor-pointer items-center gap-2.5 px-3 py-2 text-left text-[12px] transition-colors duration-150 hover:bg-white/6 ${
        danger ? 'text-danger' : 'text-ink'
      }`}
    >
      {icon}
      {label}
    </button>
  )
}

function ModuleBody({
  module,
  params,
  on,
  knob,
  onNumber,
  spectrumRef,
  metersRef,
  spectrumLive,
  metersLive,
}: {
  module: RackModule
  params: MixStationParams
  on: boolean
  knob: (id: NumericParamId, bipolar?: boolean) => React.ReactNode
  onNumber: (id: NumericParamId, value: number) => void
  spectrumRef: RefObject<SpectrumFrame | null>
  metersRef: RefObject<MeterFrame | null>
  spectrumLive: boolean
  metersLive: boolean
}) {
  switch (module.enabledId) {
    case 'filtersEnabled':
      return (
        <>
          {knob('hpfHz')}
          {knob('lpfHz')}
          <FilterCurve
            hpfHz={params.hpfHz}
            lpfHz={params.lpfHz}
            accent={module.accent}
            active={on}
          />
          <div className="flex-1" />
        </>
      )

    case 'eqEnabled':
      return (
        <>
          <BandStrip title="Low" accent={module.accent}>
            {knob('lowGainDb', true)}
            {knob('lowMidFreqHz')}
            {knob('lowMidGainDb', true)}
          </BandStrip>
          <EqCurve
            params={params}
            specs={PARAM_SPECS}
            accent={module.accent}
            active={on}
            spectrumRef={spectrumRef}
            spectrumLive={spectrumLive}
            onChange={(id, value) => onNumber(id as NumericParamId, value)}
          />
          <BandStrip title="High" accent={module.accent}>
            {knob('highMidFreqHz')}
            {knob('highMidGainDb', true)}
            {knob('highGainDb', true)}
          </BandStrip>
        </>
      )

    case 'compEnabled':
      return (
        <>
          {knob('compThresholdDb')}
          {knob('compRatio')}
          {knob('compAttackMs')}
          {knob('compReleaseMs')}
          {knob('compMakeupDb', true)}
          <CompressorCurve
            thresholdDb={params.compThresholdDb}
            ratio={params.compRatio}
            makeupDb={params.compMakeupDb}
            accent={module.accent}
            active={on}
          />
          <ReductionBar metersRef={metersRef} live={metersLive && on} accent={module.accent} />
          <div className="flex-1" />
        </>
      )

    case 'satEnabled':
      return (
        <>
          {knob('satDrivePct')}
          {knob('satCharacterPct')}
          <SaturationCurve
            drivePct={params.satDrivePct}
            characterPct={params.satCharacterPct}
            accent={module.accent}
            active={on}
          />
          <div className="flex-1" />
        </>
      )

    case 'widthEnabled':
      return (
        <>
          {knob('widthPct')}
          <WidthGraphic widthPct={params.widthPct} accent={module.accent} active={on} />
          <div className="flex-1" />
        </>
      )

    case 'limiterEnabled':
      return (
        <>
          {knob('limiterCeilingDb')}
          {knob('limiterReleaseMs')}
          <LimiterCurve
            ceilingDb={params.limiterCeilingDb}
            accent={module.accent}
            active={on}
          />
          <ReductionBar metersRef={metersRef} live={metersLive && on} accent={module.accent} />
          <div className="flex-1" />
        </>
      )
  }
}

function BandStrip({
  title,
  accent,
  children,
}: {
  title: string
  accent: string
  children: React.ReactNode
}) {
  return (
    <div className="flex shrink-0 items-center gap-1">
      <span
        className="label-cap [writing-mode:vertical-rl] rotate-180"
        style={{ color: accent }}
      >
        {title}
      </span>
      <div className="flex gap-0.5">{children}</div>
    </div>
  )
}



