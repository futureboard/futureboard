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
} from '../lib/eq'
import { Knob } from './Knob'

export type ControlRackProps = {
  band: Band
  defaultBand: Band
  selected: number
  outputDb: number
  mix: number
  onBandChange: (patch: Partial<Band>) => void
  onGlobalChange: (
    patch: Partial<Pick<EqParams, 'outputDb' | 'mix'>>,
  ) => void
}

export function ControlRack({
  band,
  defaultBand,
  selected,
  outputDb,
  mix,
  onBandChange,
  onGlobalChange,
}: ControlRackProps) {
  const kind = filterKind(band.bandType)
  const canGain = bandHasGain(band.bandType)

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
              onClick={() => onBandChange({ bandType: item.type })}
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
          onChange={(q) => onBandChange({ q })}
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
