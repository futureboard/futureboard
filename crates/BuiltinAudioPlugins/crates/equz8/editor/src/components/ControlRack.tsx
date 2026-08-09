import type { CSSProperties, Ref } from 'react'
import { HeadphonesIcon } from '@phosphor-icons/react'
import type { Band, EqParams } from '../bridge'
import {
  BAND_COLORS,
  FILTER_KINDS,
  GAIN_RANGE,
  MAX_FREQ,
  MAX_Q,
  MIN_FREQ,
  MIN_Q,
  OUTPUT_MAX_DB,
  OUTPUT_MIN_DB,
  bandHasGain,
  filterKind,
  formatFrequency,
  formatGain,
  formatQ,
  freqToProgress,
  progressToFreq,
  progressToQ,
  qToProgress,
} from '../lib/eq'
import { Knob } from './Knob'

export type ControlRackProps = {
  rackRef?: Ref<HTMLElement>
  band: Band
  defaultBand: Band
  selected: number
  outputDb: number
  mix: number
  soloed: boolean
  onBandChange: (patch: Partial<Band>) => void
  onGlobalChange: (
    patch: Partial<Pick<EqParams, 'outputDb' | 'mix'>>,
  ) => void
  onToggleSolo: () => void
}

export function ControlRack({
  rackRef,
  band,
  defaultBand,
  selected,
  outputDb,
  mix,
  soloed,
  onBandChange,
  onGlobalChange,
  onToggleSolo,
}: ControlRackProps) {
  const kind = filterKind(band.bandType)
  const canGain = bandHasGain(band.bandType)
  const canDynamic = canGain
  const accent = BAND_COLORS[selected] ?? 'var(--color-signal)'

  return (
    <section
      ref={rackRef}
      className="control-rack plane mx-3 mb-3 grid grid-cols-[minmax(11rem,1fr)_auto_auto_auto] items-center gap-x-4 gap-y-3 rounded-b-md border border-hairline bg-slot px-4 py-3 max-[780px]:grid-cols-[minmax(10rem,1fr)_auto]"
      style={{ '--band': accent } as CSSProperties}
    >
      <div className="filter-section flex min-w-0 flex-col gap-2">
        <div className="flex min-w-0 items-center gap-2">
          <span
            className="grid h-6 w-6 shrink-0 place-items-center rounded text-[11px] font-bold tabular-nums"
            style={{
              color: accent,
              background: `color-mix(in srgb, ${accent} 20%, transparent)`,
              boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${accent} 40%, transparent)`,
            }}
          >
            {selected + 1}
          </span>
          <div className="min-w-0">
            <strong className="block text-[12px] font-semibold text-ink">
              Band {selected + 1}
            </strong>
            <span className="label-cap">{kind.label}</span>
          </div>
          <button
            type="button"
            className="ml-auto grid h-[22px] w-[22px] shrink-0 cursor-pointer place-items-center rounded border transition-colors duration-120"
            style={{
              borderColor: soloed ? accent : 'var(--color-hairline-hi)',
              background: soloed ? accent : 'transparent',
              color: soloed ? '#071014' : 'var(--color-ink-dim)',
            }}
            aria-pressed={soloed}
            aria-label={`Listen to band ${selected + 1} alone`}
            title={soloed ? 'Stop listening (Esc)' : "Listen to this band alone"}
            onClick={onToggleSolo}
          >
            <HeadphonesIcon size={12} weight="bold" />
          </button>
          <button
            type="button"
            className="h-[22px] shrink-0 cursor-pointer rounded border px-1.5 text-[9px] font-bold tracking-[0.1em] uppercase transition-colors duration-120 disabled:cursor-not-allowed disabled:opacity-35"
            style={{
              borderColor: band.dynamic ? accent : 'var(--color-hairline-hi)',
              background: band.dynamic ? accent : 'transparent',
              color: band.dynamic ? '#071014' : 'var(--color-ink-dim)',
            }}
            aria-pressed={band.dynamic}
            disabled={!canDynamic}
            aria-label={`Band ${selected + 1} dynamic EQ`}
            title={
              canDynamic
                ? band.dynamic
                  ? 'Disable dynamic EQ'
                  : 'Enable dynamic EQ'
                : `${kind.label} has no gain stage for dynamic EQ`
            }
            onClick={() => {
              if (!canDynamic) return
              onBandChange({ dynamic: !band.dynamic })
            }}
          >
            Dyn
          </button>
          <button
            type="button"
            role="switch"
            aria-checked={band.active}
            aria-label={`Band ${selected + 1} enabled`}
            title={band.active ? 'Disable band' : 'Enable band'}
            className="h-2.5 w-2.5 shrink-0 cursor-pointer rounded-full border transition-colors duration-120"
            style={{
              borderColor: band.active ? accent : 'var(--color-ink-dim)',
              background: band.active ? accent : 'transparent',
              boxShadow: band.active
                ? `0 0 8px color-mix(in srgb, ${accent} 55%, transparent)`
                : undefined,
            }}
            onClick={() => onBandChange({ active: !band.active })}
          />
        </div>

        <div className="grid grid-cols-6 gap-0.5">
          {FILTER_KINDS.map((item) => {
            const active = band.bandType === item.type
            return (
              <button
                key={item.type}
                type="button"
                className="grid h-[30px] min-w-0 cursor-pointer place-items-center rounded border bg-black/20 transition-colors duration-120 hover:border-hairline-hi hover:bg-white/4"
                style={{
                  borderColor: active
                    ? `color-mix(in srgb, ${accent} 42%, transparent)`
                    : 'transparent',
                  background: active
                    ? `color-mix(in srgb, ${accent} 14%, transparent)`
                    : undefined,
                }}
                aria-pressed={active}
                title={item.label}
                aria-label={item.label}
                onClick={() =>
                  onBandChange({
                    bandType: item.type,
                    ...(bandHasGain(item.type) ? {} : { dynamic: false }),
                  })
                }
              >
                <svg viewBox="0 0 32 20" className="h-4 w-[82%]" aria-hidden="true">
                  <path
                    d={item.glyph}
                    fill="none"
                    stroke={active ? accent : 'var(--color-ink-dim)'}
                    strokeWidth="1.45"
                    strokeLinecap="round"
                  />
                </svg>
              </button>
            )
          })}
        </div>
      </div>

      <div className="band-controls grid grid-cols-3 justify-center gap-1">
        <Knob
          variant="band"
          label="Freq"
          value={band.freq}
          min={MIN_FREQ}
          max={MAX_FREQ}
          step={1}
          unit="Hz"
          format={formatFrequency}
          defaultValue={defaultBand.freq}
          toProgress={freqToProgress}
          fromProgress={progressToFreq}
          onChange={(freq) => onBandChange({ freq })}
        />
        <Knob
          variant="band"
          label="Gain"
          value={band.gainDb}
          min={-GAIN_RANGE}
          max={GAIN_RANGE}
          step={0.1}
          unit="dB"
          format={formatGain}
          defaultValue={defaultBand.gainDb}
          originAtDefault
          disabled={!canGain}
          disabledHint={`${kind.label} has no gain stage`}
          onChange={(gainDb) => onBandChange({ gainDb })}
        />
        <Knob
          variant="band"
          label="Q"
          value={band.q}
          min={MIN_Q}
          max={MAX_Q}
          step={0.01}
          format={formatQ}
          defaultValue={defaultBand.q}
          toProgress={qToProgress}
          fromProgress={progressToQ}
          onChange={(q) => onBandChange({ q })}
        />
      </div>

      <div
        className={`dyn-controls grid grid-cols-4 justify-center gap-1 transition-opacity duration-150 ${
          band.dynamic && canDynamic
            ? 'opacity-100'
            : 'pointer-events-none opacity-35'
        }`}
      >
        <span className="label-cap col-span-4 text-center">Dynamic</span>
        <Knob
          variant="band"
          label="Thresh"
          value={band.thresholdDb}
          min={-60}
          max={0}
          step={0.5}
          unit="dB"
          format={formatGain}
          defaultValue={defaultBand.thresholdDb}
          disabled={!band.dynamic || !canDynamic}
          disabledHint="Enable Dyn first"
          onChange={(thresholdDb) => onBandChange({ thresholdDb })}
        />
        <Knob
          variant="band"
          label="Range"
          value={band.rangeDb}
          min={-24}
          max={24}
          step={0.1}
          unit="dB"
          format={formatGain}
          defaultValue={defaultBand.rangeDb}
          originAtDefault
          disabled={!band.dynamic || !canDynamic}
          disabledHint="Enable Dyn first"
          onChange={(rangeDb) => onBandChange({ rangeDb })}
        />
        <Knob
          variant="band"
          label="Attack"
          value={band.attackMs}
          min={0.1}
          max={500}
          step={0.1}
          unit="ms"
          format={(value) =>
            value < 10 ? value.toFixed(1) : String(Math.round(value))
          }
          defaultValue={defaultBand.attackMs}
          disabled={!band.dynamic || !canDynamic}
          disabledHint="Enable Dyn first"
          onChange={(attackMs) => onBandChange({ attackMs })}
        />
        <Knob
          variant="band"
          label="Release"
          value={band.releaseMs}
          min={1}
          max={5000}
          step={1}
          unit="ms"
          format={(value) => String(Math.round(value))}
          defaultValue={defaultBand.releaseMs}
          disabled={!band.dynamic || !canDynamic}
          disabledHint="Enable Dyn first"
          onChange={(releaseMs) => onBandChange({ releaseMs })}
        />
      </div>

      <div className="master-section flex flex-col justify-center gap-2 self-stretch border-l border-hairline-hi pl-4 max-[780px]:col-span-2 max-[780px]:flex-row max-[780px]:items-center max-[780px]:border-t max-[780px]:border-l-0 max-[780px]:pt-3 max-[780px]:pl-0">
        <span className="label-cap">Output</span>
        <div className="master-controls grid grid-cols-2 justify-center gap-1">
          <Knob
            variant="master"
            label="Level"
            value={outputDb}
            min={OUTPUT_MIN_DB}
            max={OUTPUT_MAX_DB}
            step={0.1}
            unit="dB"
            format={formatGain}
            defaultValue={0}
            originAtDefault
            onChange={(value) => onGlobalChange({ outputDb: value })}
          />
          <Knob
            variant="master"
            label="Mix"
            value={mix}
            min={0}
            max={100}
            step={1}
            unit="%"
            format={(value) => `${Math.round(value)}`}
            defaultValue={100}
            onChange={(value) => onGlobalChange({ mix: value })}
          />
        </div>
      </div>
    </section>
  )
}
