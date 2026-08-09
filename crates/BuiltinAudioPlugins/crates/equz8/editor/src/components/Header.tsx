import logo from '../assets/logo.svg'

export type HeaderProps = {
  connected: boolean
  power: boolean
  preset: number | null
  presetNames: string[]
  onPresetChange: (index: number) => void
  onPreviousPreset: () => void
  onNextPreset: () => void
  onReset: () => void
  onPowerChange: (power: boolean) => void
}

export function Header({
  connected,
  power,
  preset,
  presetNames,
  onPresetChange,
  onPreviousPreset,
  onNextPreset,
  onReset,
  onPowerChange,
}: HeaderProps) {
  return (
    <header className="editor-header">
      <div className="brand">
        <img src={logo} alt="Futureboard EQUZ8" />
        <div className="brand-mark">
          <span className="brand-title">EQUZ8</span>
          <span className="brand-sub">Dynamic EQ</span>
        </div>
        <span
          className="connection"
          data-live={connected}
          title={
            connected
              ? 'Linked to the DSP instance'
              : 'Preview — no DSP instance bound'
          }
        />
      </div>

      <div className="preset-control">
        <button
          type="button"
          className="preset-step"
          aria-label="Previous preset"
          onClick={onPreviousPreset}
        >
          ‹
        </button>
        <label className="preset-select">
          <span>{preset === null ? 'Custom' : presetNames[preset]}</span>
          <select
            aria-label="Preset"
            value={preset ?? 'custom'}
            onChange={(event) => {
              if (event.target.value !== 'custom') {
                onPresetChange(Number(event.target.value))
              }
            }}
          >
            {preset === null && <option value="custom">Custom</option>}
            {presetNames.map((name, index) => (
              <option key={name} value={index}>
                {name}
              </option>
            ))}
          </select>
        </label>
        <button
          type="button"
          className="preset-step"
          aria-label="Next preset"
          onClick={onNextPreset}
        >
          ›
        </button>
      </div>

      <div className="header-actions">
        <button
          type="button"
          className="icon-button"
          title="Load default preset"
          aria-label="Load default preset"
          onClick={onReset}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M20 12a8 8 0 1 1-2.5-5.8M20 4v3.6h-3.7" />
          </svg>
        </button>
        <button
          type="button"
          className={`icon-button power-button${power ? ' is-on' : ''}`}
          aria-label={power ? 'Bypass equalizer' : 'Enable equalizer'}
          aria-pressed={!power}
          title={power ? 'Bypass' : 'Enable'}
          onClick={() => onPowerChange(!power)}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M12 3.5v8.5m5.9-6.1a8.4 8.4 0 1 1-11.8 0" />
          </svg>
        </button>
      </div>
    </header>
  )
}
