import type { CSSProperties } from 'react'
import type { Band } from '../bridge'
import { BAND_COLORS, filterKind, formatFrequency } from '../lib/eq'

export type BandChipsProps = {
  bands: Band[]
  selected: number
  onSelect: (index: number) => void
  onToggle: (index: number) => void
}

/// Compact band picker for the cases the curve cannot serve: overlapping nodes,
/// and switching a band off without hunting for its node.
export function BandChips({
  bands,
  selected,
  onSelect,
  onToggle,
}: BandChipsProps) {
  return (
    <div className="band-chips">
      {bands.map((band, index) => (
        <button
          key={index}
          type="button"
          className={`band-chip${selected === index ? ' is-selected' : ''}${
            band.active ? '' : ' is-off'
          }`}
          style={{ '--band': BAND_COLORS[index] } as CSSProperties}
          aria-pressed={selected === index}
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
      ))}
    </div>
  )
}
