import {
  BAND_CHANNELS,
  BAND_TYPES,
  MAX_BANDS,
  SLOPES,
  makeBand,
  type Band,
  type BandChannel,
  type BandType,
} from '../dsp/bands'

/** Everything an A/B slot or a preset carries. */
export interface Snapshot {
  bands: Band[]
  outputGain: number
}

export interface Preset extends Snapshot {
  name: string
  version: number
}

export const PRESET_VERSION = 1
const STORAGE_KEY = 'equzfree.presets.v1'

const VALID_TYPES = new Set<string>(BAND_TYPES.map((t) => t.value))
const VALID_CHANNELS = new Set<string>(BAND_CHANNELS.map((c) => c.value))
const VALID_SLOPES = new Set<number>(SLOPES)

function num(v: unknown, fallback: number, lo: number, hi: number): number {
  const n = typeof v === 'number' ? v : Number(v)
  if (!Number.isFinite(n)) return fallback
  return Math.min(Math.max(n, lo), hi)
}

function bool(v: unknown, fallback: boolean): boolean {
  return typeof v === 'boolean' ? v : fallback
}

/**
 * Rebuild a band from untrusted input. Presets can come from a file the user
 * edited by hand, and an out-of-range or missing value would otherwise reach
 * Web Audio and throw — every field is clamped, and ids are reissued so an
 * imported preset can never collide with a live band.
 */
export function sanitizeBand(raw: unknown): Band {
  const o = (raw ?? {}) as Record<string, unknown>
  const type: BandType = VALID_TYPES.has(o.type as string) ? (o.type as BandType) : 'bell'
  const slope = VALID_SLOPES.has(Number(o.slope)) ? Number(o.slope) : 24
  // Presets written before mid/side existed carry no channel — those are stereo.
  const channel: BandChannel = VALID_CHANNELS.has(o.channel as string)
    ? (o.channel as BandChannel)
    : 'stereo'

  return makeBand({
    type,
    channel,
    slope,
    freq: num(o.freq, 1000, 20, 22000),
    gain: num(o.gain, 0, -30, 30),
    q: num(o.q, 1, 0.025, 40),
    enabled: bool(o.enabled, true),
    dynamic: bool(o.dynamic, false),
    dynMode: o.dynMode === 'below' ? 'below' : 'above',
    dynRange: num(o.dynRange, -6, -30, 30),
    threshold: num(o.threshold, -24, -70, 0),
    attack: num(o.attack, 20, 1, 300),
    release: num(o.release, 200, 10, 2000),
  })
}

export function sanitizeSnapshot(raw: unknown): Snapshot {
  const o = (raw ?? {}) as Record<string, unknown>
  const list = Array.isArray(o.bands) ? o.bands : []
  return {
    bands: list.slice(0, MAX_BANDS).map(sanitizeBand),
    outputGain: num(o.outputGain, 0, -24, 12),
  }
}

/** Deep copy with fresh ids — used when forking a slot so the two can't alias. */
export function cloneSnapshot(snap: Snapshot): Snapshot {
  return {
    bands: snap.bands.map((b) => makeBand({ ...b })),
    outputGain: snap.outputGain,
  }
}

export function emptySnapshot(): Snapshot {
  return { bands: [], outputGain: 0 }
}

// --- persistence ---------------------------------------------------------

type Store = Record<string, Preset>

function readStore(): Store {
  try {
    const raw = localStorage.getItem(STORAGE_KEY)
    if (!raw) return {}
    const parsed: unknown = JSON.parse(raw)
    if (!parsed || typeof parsed !== 'object') return {}
    return parsed as Store
  } catch {
    // Corrupt entry or storage blocked (private mode) — start clean rather than break the UI.
    return {}
  }
}

function writeStore(store: Store): boolean {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(store))
    return true
  } catch {
    return false
  }
}

export function listPresets(): string[] {
  return Object.keys(readStore()).sort((a, b) => a.localeCompare(b))
}

export function savePreset(name: string, snap: Snapshot): boolean {
  const trimmed = name.trim()
  if (!trimmed) return false
  const store = readStore()
  store[trimmed] = { name: trimmed, version: PRESET_VERSION, ...sanitizeSnapshot(snap) }
  return writeStore(store)
}

export function loadPreset(name: string): Snapshot | null {
  const preset = readStore()[name]
  return preset ? sanitizeSnapshot(preset) : null
}

export function deletePreset(name: string): boolean {
  const store = readStore()
  if (!(name in store)) return false
  delete store[name]
  return writeStore(store)
}

// --- file exchange -------------------------------------------------------

export function exportPreset(name: string, snap: Snapshot) {
  const preset: Preset = { name, version: PRESET_VERSION, ...sanitizeSnapshot(snap) }
  const blob = new Blob([JSON.stringify(preset, null, 2)], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = `${name.replace(/[^\w.-]+/g, '_') || 'preset'}.equz.json`
  a.click()
  URL.revokeObjectURL(url)
}

export async function importPreset(file: File): Promise<{ name: string; snapshot: Snapshot }> {
  const text = await file.text()
  const parsed: unknown = JSON.parse(text)
  const o = (parsed ?? {}) as Record<string, unknown>
  const name = typeof o.name === 'string' && o.name.trim() ? o.name.trim() : file.name.replace(/\.\w+$/, '')
  return { name, snapshot: sanitizeSnapshot(parsed) }
}
