type PresetControlProps = {
  preset: number | null
  names: string[]
  onChange: (index: number) => void
  onPrevious: () => void
  onNext: () => void
}

export function PresetControl({
  preset,
  names,
  onChange,
  onPrevious,
  onNext,
}: PresetControlProps) {
  return (
    <div className="preset">
      <button
        type="button"
        className="step"
        aria-label="Previous preset"
        onClick={onPrevious}
      >
        ‹
      </button>
      <label className="select">
        <span>{preset === null ? 'Custom' : names[preset]}</span>
        <select
          aria-label="Preset"
          value={preset ?? 'custom'}
          onChange={(event) => {
            const value = event.currentTarget.value
            if (value !== 'custom') onChange(Number(value))
          }}
        >
          {preset === null ? <option value="custom">Custom</option> : null}
          {names.map((name, index) => (
            <option key={name} value={index}>
              {name}
            </option>
          ))}
        </select>
      </label>
      <button
        type="button"
        className="step"
        aria-label="Next preset"
        onClick={onNext}
      >
        ›
      </button>
    </div>
  )
}
