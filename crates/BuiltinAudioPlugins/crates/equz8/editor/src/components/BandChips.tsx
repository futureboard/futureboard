import type { CSSProperties } from 'react'
import type { Band } from '../bridge'
import { BAND_COLORS, filterKind, formatFrequency } from '../lib/eq'

export type BandChipsProps = {
  bands: Band[]
  selected: number
  onSelect: (index: number) => void
  onToggle: (index: number) => void
}

export function BandChips({
  bands,
  selected,
  onSelect,
  onToggle,
}: BandChipsProps) {
  return (
    <div className="band-chips flex gap-0.5 rounded border border-hairline bg-well/70 p-0.5 backdrop-blur-sm">
      {bands.map((band, index) => {
        const active = selected === index
        return (
          <button
            key={index}
            type="button"
            className={`grid h-[22px] w-[22px] cursor-pointer place-items-center rounded text-[10px] font-semibold tabular-nums transition-colors duration-120 ${
              band.active ? '' : 'line-through opacity-40'
            }`}
            style={
              {
                '--band': BAND_COLORS[index],
                color: band.active ? BAND_COLORS[index] : 'var(--color-ink-dim)',
                background: active
                  ? `color-mix(in srgb, ${BAND_COLORS[index]} 22%, transparent)`
                  : undefined,
                boxShadow: active
                  ? `inset 0 0 0 1px color-mix(in srgb, ${BAND_COLORS[index]} 45%, transparent)`
                  : undefined,
                opacity: active || !band.active ? undefined : 0.55,
              } as CSSProperties
            }
            aria-pressed={active}
            title={`${filterKind(band.bandType).label} at ${formatFrequency(
              band.freq,
            )} Hz — click to select, double-click to ${
              band.active ? 'disable' : 'enable'
            }`}
            onClick={() => onSelect(index)}
            onDoubleClick={() => onToggle(index)}
          >
            {index + 1}
          </button>
        )
      })}
    </div>
  )
}
