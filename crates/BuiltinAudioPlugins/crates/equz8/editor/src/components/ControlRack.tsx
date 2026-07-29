import type { CSSProperties } from 'react'
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

  return (
    <section
      className="control-rack"
      style={{ '--band': BAND_COLORS[selected] } as CSSProperties}
    >
      <div className="filter-section">
        <div className="filter-heading">
          <span className="band-badge">{selected + 1}</span>
          <div>
            <strong>Band {selected + 1}</strong>
            <span>{kind.label}</span>
          </div>
          <button
            type="button"
            className={`band-solo${soloed ? ' is-on' : ''}`}
            aria-pressed={soloed}
            aria-label={`Listen to band ${selected + 1} alone`}
            title={
              soloed
                ? 'Stop listening (Esc)'
                : "Listen to this band's frequency range alone"
            }
            onClick={onToggleSolo}
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M4 14v-2a8 8 0 0 1 16 0v2" />
              <path d="M4 14h3v5H5a1 1 0 0 1-1-1zM20 14h-3v5h2a1 1 0 0 0 1-1z" />
            </svg>
          </button>
          <button
            type="button"
            className={`band-dyn${band.dynamic ? ' is-on' : ''}${canDynamic ? '' : ' is-disabled'}`}
            aria-pressed={band.dynamic}
            disabled={!canDynamic}
            aria-label={`Band ${selected + 1} dynamic EQ`}
            title={
              canDynamic
                ? band.dynamic
                  ? 'Disable dynamic EQ'
                  : 'Enable dynamic EQ (Pro-Q style)'
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
            className={`band-toggle${band.active ? ' is-on' : ''}`}
            role="switch"
            aria-checked={band.active}
            aria-label={`Band ${selected + 1} enabled`}
            title={band.active ? 'Disable band' : 'Enable band'}
            onClick={() => onBandChange({ active: !band.active })}
          />
        </div>

        <div className="filter-types">
          {FILTER_KINDS.map((item) => (
            <button
              key={item.type}
              type="button"
              className={`filter-type${band.bandType === item.type ? ' is-active' : ''}`}
              aria-pressed={band.bandType === item.type}
              title={item.label}
              aria-label={item.label}
              onClick={() =>
                onBandChange({
                  bandType: item.type,
                  ...(bandHasGain(item.type) ? {} : { dynamic: false }),
                })
              }
            >
              <svg viewBox="0 0 32 20" aria-hidden="true">
                <path d={item.glyph} />
              </svg>
            </button>
          ))}
        </div>
      </div>

      <div className="band-controls">
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

      <div className={`dyn-controls${band.dynamic && canDynamic ? ' is-open' : ''}`}>
        <span className="dyn-label">Dynamic</span>
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

      <div className="master-section">
        <span className="master-label">Output</span>
        <div className="master-controls">
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
