import { useRef, useState } from 'react'
import { Chevron, LABEL, Menu, MenuDivider, MenuItem, MenuLabel } from './ui/Menu'
import {
  deletePreset,
  exportPreset,
  importPreset,
  listPresets,
  loadPreset,
  savePreset,
  type Snapshot,
} from '../state/presets'

interface Props {
  /** Current settings, read lazily when the user saves or exports. */
  getSnapshot: () => Snapshot
  onLoad: (snap: Snapshot, name: string) => void
}

export function PresetMenu({ getSnapshot, onLoad }: Props) {
  const [names, setNames] = useState<string[]>(listPresets)
  const [current, setCurrent] = useState('')
  const [draft, setDraft] = useState('')
  const [status, setStatus] = useState('')
  const fileRef = useRef<HTMLInputElement>(null)

  const flash = (msg: string) => {
    setStatus(msg)
    window.setTimeout(() => setStatus(''), 2000)
  }

  const handleSave = () => {
    const name = (draft.trim() || current).trim()
    if (!name) return flash('Name it first')
    if (!savePreset(name, getSnapshot())) return flash('Could not save')
    setNames(listPresets())
    setCurrent(name)
    setDraft('')
    flash('Saved')
  }

  const handleLoad = (name: string) => {
    const snap = loadPreset(name)
    if (!snap) return flash('Not found')
    setCurrent(name)
    onLoad(snap, name)
  }

  const handleDelete = (name: string) => {
    deletePreset(name)
    setNames(listPresets())
    if (current === name) setCurrent('')
    flash('Deleted')
  }

  const handleImport = async (file: File) => {
    try {
      const { name, snapshot } = await importPreset(file)
      onLoad(snapshot, name)
      setCurrent(name)
      setDraft(name)
      flash('Imported')
    } catch {
      flash('Invalid preset file')
    }
  }

  return (
    <>
      <Menu
        panelClass="w-[248px]"
        title="Presets"
        trigger={(open) => (
          <>
            <span className={LABEL}>Preset</span>
            <span className="max-w-[120px] truncate font-medium text-white/90">
              {status || current || 'Init'}
            </span>
            <Chevron open={open} />
          </>
        )}
      >
        {(close) => (
          <>
            <MenuLabel>{names.length ? 'Saved' : 'No presets yet'}</MenuLabel>
            {names.length > 0 && (
              <div className="max-h-56 overflow-y-auto">
                {names.map((n) => (
                  <div key={n} className="group/row relative">
                    <MenuItem
                      selected={n === current}
                      onClick={() => {
                        handleLoad(n)
                        close()
                      }}
                    >
                      {n}
                    </MenuItem>
                    <button
                      type="button"
                      title={`Delete "${n}"`}
                      onClick={(ev) => {
                        ev.stopPropagation()
                        handleDelete(n)
                      }}
                      className="absolute right-1.5 top-1/2 hidden -translate-y-1/2 rounded p-1 text-white/40 transition hover:bg-red-500/20 hover:text-red-300 group-hover/row:block"
                    >
                      <svg width={10} height={10} viewBox="0 0 10 10">
                        <path d="M1.5 1.5 8.5 8.5M8.5 1.5 1.5 8.5" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round" />
                      </svg>
                    </button>
                  </div>
                ))}
              </div>
            )}

            <MenuDivider />
            <MenuLabel>Save current</MenuLabel>
            <div className="flex gap-1 px-1 pb-1">
              <input
                value={draft}
                onChange={(e) => setDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') handleSave()
                }}
                placeholder={current || 'Preset name'}
                className="h-7 min-w-0 flex-1 rounded-full border border-white/10 bg-black/40 px-3 text-[11px] text-white/85 placeholder:text-white/25 outline-none focus:border-neon/60"
              />
              <button
                type="button"
                onClick={handleSave}
                className="neon-solid h-7 shrink-0 rounded-full px-3 text-[10px] font-semibold uppercase tracking-wide transition hover:bg-neon-soft"
              >
                Save
              </button>
            </div>

            <MenuDivider />
            <MenuItem
              onClick={() => {
                exportPreset(current || draft.trim() || 'preset', getSnapshot())
                close()
              }}
            >
              Export to file…
            </MenuItem>
            <MenuItem
              onClick={() => {
                fileRef.current?.click()
                close()
              }}
            >
              Import from file…
            </MenuItem>
          </>
        )}
      </Menu>

      <input
        ref={fileRef}
        type="file"
        accept=".json,application/json"
        className="hidden"
        onChange={(e) => {
          const f = e.target.files?.[0]
          if (f) void handleImport(f)
          e.target.value = ''
        }}
      />
    </>
  )
}
