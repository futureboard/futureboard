import { useEffect, useRef } from 'react'
import { animate } from 'animejs'
import { Knob } from './Knob'
import type { AudioEngine } from '../audio/AudioEngine'
import { NEON } from '../theme'
import {
  BAND_CHANNELS,
  BAND_TYPES,
  IS_CUT,
  MAX_BANDS,
  SLOPES,
  USES_GAIN,
  USES_Q,
  bandColor,
  canBeDynamic,
  type Band,
  type BandType,
} from '../dsp/bands'

interface Props {
  bands: Band[]
  selectedId: number | null
  soloId: number | null
  engine: AudioEngine | null
  onSelect: (id: number | null) => void
  onPatch: (id: number, patch: Partial<Band>) => void
  onRemove: (id: number) => void
  onSolo: (id: number | null) => void
  height: number
}

function fmtFreq(f: number) {
  return f >= 1000
    ? `${(f / 1000).toFixed(f >= 10000 ? 1 : 2)} kHz`
    : `${f.toFixed(f < 100 ? 1 : 0)} Hz`
}

export function BandStrip({
  bands,
  selectedId,
  soloId,
  engine,
  onSelect,
  onPatch,
  onRemove,
  onSolo,
  height,
}: Props) {
  const band = bands.find((b) => b.id === selectedId) ?? null
  const index = bands.findIndex((b) => b.id === selectedId)
  const color = index >= 0 ? bandColor(index) : NEON
  const panelRef = useRef<HTMLDivElement>(null)
  const full = bands.length >= MAX_BANDS

  // Animate only when the selection actually changes — keying this on the band
  // object would replay the transition on every knob movement.
  useEffect(() => {
    if (!panelRef.current || selectedId === null) return
    animate(panelRef.current, {
      opacity: [0, 1],
      translateY: [6, 0],
      duration: 240,
      ease: 'outQuad',
    })
  }, [selectedId])

  const dynAllowed = band ? canBeDynamic(band.type) : false
  const dynOn = !!band?.dynamic && dynAllowed

  return (
    <div
      className="flex shrink-0 items-stretch gap-3 overflow-hidden px-3 py-2.5"
      style={{ height }}
    >
      {/* band chips */}
      <div className="flex w-[188px] shrink-0 flex-col gap-1.5">
        <div className="flex min-h-0 flex-1 flex-wrap content-start gap-1.5 overflow-y-auto">
          {bands.length === 0 && (
            <span className="text-[10px] leading-7 text-white/25">No bands yet</span>
          )}
          {bands.map((b, i) => {
            const active = b.id === selectedId
            return (
              <button
                key={b.id}
                onClick={() => onSelect(b.id)}
                title={`${BAND_TYPES.find((t) => t.value === b.type)?.label} · ${fmtFreq(b.freq)}${
                  b.channel === 'stereo' ? '' : ` · ${b.channel}`
                }`}
                className={`relative h-7 w-7 rounded-md border text-[11px] font-semibold tabular-nums transition ${
                  active
                    ? 'border-white/40 text-black'
                    : 'border-white/10 text-white/60 hover:border-white/25'
                }`}
                style={{
                  background: active
                    ? bandColor(i)
                    : b.enabled
                      ? 'rgba(255,255,255,0.04)'
                      : 'transparent',
                  opacity: b.enabled ? 1 : 0.45,
                  boxShadow: active
                    ? `0 0 0 1px ${bandColor(i)}55, 0 0 12px ${bandColor(i)}40`
                    : undefined,
                }}
              >
                {i + 1}
                {b.dynamic && canBeDynamic(b.type) && (
                  <span
                    className="absolute -right-0.5 -top-0.5 h-1.5 w-1.5 rounded-full"
                    style={{ background: active ? '#000' : bandColor(i) }}
                  />
                )}
                {b.channel !== 'stereo' && (
                  <span
                    className="absolute bottom-0 right-0.5 text-[7px] font-bold leading-none"
                    style={{ color: active ? '#000' : bandColor(i) }}
                  >
                    {b.channel === 'mid' ? 'M' : 'S'}
                  </span>
                )}
              </button>
            )
          })}
        </div>
        <div className={`text-[9px] tabular-nums ${full ? 'text-neon' : 'text-white/25'}`}>
          {bands.length} / {MAX_BANDS} bands{full ? ' — limit reached' : ''}
        </div>
      </div>

      <div className="w-px bg-white/12" />

      {!band ? (
        <div className="flex flex-1 items-center px-2 text-[11px] text-white/30">
          Click anywhere on the display to create a band.
        </div>
      ) : (
        <div ref={panelRef} className="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto">
          {/* --- row 1: filter --- */}
          <div className="flex items-center gap-4">
            <div className="flex items-center gap-1">
              {BAND_TYPES.map((t) => (
                <button
                  key={t.value}
                  onClick={() => onPatch(band.id, { type: t.value as BandType })}
                  title={t.label}
                  className={`grid h-8 w-8 place-items-center rounded-xl transition ${
                    band.type === t.value ? 'glass-pill glass-pill-on' : 'glass-pill'
                  }`}
                >
                  <svg width={20} height={17} viewBox="0 0 20 17" fill="none">
                    <path
                      d={t.glyph}
                      stroke={band.type === t.value ? color : 'rgba(255,255,255,0.45)'}
                      strokeWidth={1.6}
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    />
                  </svg>
                </button>
              ))}
            </div>

            <div className="h-10 w-px bg-white/8" />

            <Knob
              label="Freq"
              value={band.freq}
              min={20}
              max={22000}
              scale="log"
              color={color}
              format={fmtFreq}
              onChange={(v) => onPatch(band.id, { freq: v })}
            />
            <Knob
              label="Gain"
              value={band.gain}
              min={-30}
              max={30}
              defaultValue={0}
              color={color}
              disabled={!USES_GAIN[band.type]}
              format={(v) => `${v >= 0 ? '+' : ''}${v.toFixed(1)} dB`}
              onChange={(v) => onPatch(band.id, { gain: v })}
            />
            <Knob
              label="Q"
              value={band.q}
              min={0.025}
              max={40}
              scale="log"
              defaultValue={1}
              decimals={2}
              color={color}
              disabled={!USES_Q[band.type]}
              onChange={(v) => onPatch(band.id, { q: v })}
            />

            <div className="flex flex-col gap-1">
              <div className="text-[9px] uppercase tracking-wider text-white/35">Slope</div>
              <div className={`flex gap-1 ${IS_CUT[band.type] ? '' : 'pointer-events-none opacity-25'}`}>
                {SLOPES.map((s) => (
                  <button
                    key={s}
                    onClick={() => onPatch(band.id, { slope: s })}
                    className={`rounded-full px-2 py-1 text-[10px] tabular-nums transition ${
                      band.slope === s ? 'text-black' : 'glass-pill text-white/55'
                    }`}
                    style={{ background: band.slope === s ? color : undefined }}
                  >
                    {s}
                  </button>
                ))}
              </div>
              <div className="text-[9px] text-white/25">dB / oct</div>
            </div>

            <div className="flex flex-col gap-1">
              <div className="text-[9px] uppercase tracking-wider text-white/35">Channel</div>
              <div className="glass-pill flex overflow-hidden rounded-full">
                {BAND_CHANNELS.map((c) => (
                  <button
                    key={c.value}
                    onClick={() => onPatch(band.id, { channel: c.value })}
                    title={`${c.label} — ${
                      c.value === 'stereo' ? 'both channels' : `the ${c.label.toLowerCase()} signal only`
                    }`}
                    className={`px-2.5 py-1.5 text-[10px] transition ${
                      band.channel === c.value
                        ? 'text-black'
                        : 'text-white/45 hover:bg-white/8 hover:text-white/80'
                    }`}
                    style={{ background: band.channel === c.value ? color : undefined }}
                  >
                    {c.short}
                  </button>
                ))}
              </div>
              <div className="text-[9px] text-white/25">mid / side</div>
            </div>

            <div className="ml-auto flex items-center gap-1.5">
              <button
                onClick={() => onPatch(band.id, { enabled: !band.enabled })}
                className={`h-8 rounded-full px-3 text-[10px] uppercase tracking-wide transition ${
                  band.enabled ? 'glass-pill glass-pill-on text-white/90' : 'glass-pill text-white/35'
                }`}
              >
                {band.enabled ? 'On' : 'Off'}
              </button>
              <button
                onClick={() => onSolo(soloId === band.id ? null : band.id)}
                title="Solo this band — or right-drag its handle on the display"
                className={`h-8 rounded-full px-3 text-[10px] uppercase tracking-wide transition ${
                  soloId === band.id
                    ? 'mochi-on'
                    : 'glass-pill text-white/45 hover:text-white/80'
                }`}
              >
                Solo
              </button>
              <button
                onClick={() => onRemove(band.id)}
                className="glass-pill h-8 rounded-full px-3 text-[10px] uppercase tracking-wide text-white/45 transition hover:text-red-300"
              >
                Del
              </button>
            </div>
          </div>

          {/* --- row 2: dynamics --- */}
          <div className="flex items-center gap-3 rounded-2xl bg-white/4 px-2.5 py-2 shadow-[inset_0_1px_0_rgba(255,255,255,0.08),inset_0_0_0_1px_rgba(255,255,255,0.06)]">
            <button
              onClick={() => onPatch(band.id, { dynamic: !band.dynamic })}
              disabled={!dynAllowed}
              title={dynAllowed ? 'Dynamic mode' : 'Dynamics need a band with gain (bell or shelf)'}
              className={`h-8 rounded-full px-3 text-[10px] uppercase tracking-wide transition disabled:opacity-25 ${
                dynOn ? 'text-black' : 'glass-pill text-white/45 hover:text-white/80'
              }`}
              style={{ background: dynOn ? color : undefined, borderColor: dynOn ? color : undefined }}
            >
              Dyn
            </button>

            <div className={`flex flex-1 items-center gap-3 ${dynOn ? '' : 'pointer-events-none opacity-25'}`}>
              <div className="glass-pill flex overflow-hidden rounded-full">
                {(['above', 'below'] as const).map((m) => (
                  <button
                    key={m}
                    onClick={() => onPatch(band.id, { dynMode: m })}
                    className={`px-2.5 py-1.5 text-[10px] capitalize transition ${
                      band.dynMode === m ? 'bg-white/15 text-white/90' : 'text-white/40 hover:bg-white/8'
                    }`}
                  >
                    {m}
                  </button>
                ))}
              </div>

              <Knob
                label="Range"
                value={band.dynRange}
                min={-30}
                max={30}
                defaultValue={-6}
                color={color}
                format={(v) => `${v >= 0 ? '+' : ''}${v.toFixed(1)} dB`}
                onChange={(v) => onPatch(band.id, { dynRange: v })}
              />
              <Knob
                label="Thresh"
                value={band.threshold}
                min={-70}
                max={0}
                defaultValue={-24}
                color={color}
                format={(v) => `${v.toFixed(1)} dB`}
                onChange={(v) => onPatch(band.id, { threshold: v })}
              />
              <Knob
                label="Attack"
                value={band.attack}
                min={1}
                max={300}
                scale="log"
                defaultValue={20}
                color={color}
                format={(v) => `${v.toFixed(0)} ms`}
                onChange={(v) => onPatch(band.id, { attack: v })}
              />
              <Knob
                label="Release"
                value={band.release}
                min={10}
                max={2000}
                scale="log"
                defaultValue={200}
                color={color}
                format={(v) => `${v.toFixed(0)} ms`}
                onChange={(v) => onPatch(band.id, { release: v })}
              />

              <DynMeter band={band} engine={engine} color={color} active={dynOn} />
            </div>
          </div>
        </div>
      )}
    </div>
  )
}

const METER_MIN = -70

/** Live band level against the threshold, plus the gain the band is currently applying. */
function DynMeter({
  band,
  engine,
  color,
  active,
}: {
  band: Band
  engine: AudioEngine | null
  color: string
  active: boolean
}) {
  const fillRef = useRef<HTMLDivElement>(null)
  const deltaRef = useRef<HTMLSpanElement>(null)

  useEffect(() => {
    if (!engine || !active) return
    let raf = 0
    const tick = () => {
      raf = requestAnimationFrame(tick)
      const level = engine.getLevel(band.id)
      const pct = Math.min(Math.max((level - METER_MIN) / -METER_MIN, 0), 1) * 100
      if (fillRef.current) fillRef.current.style.width = `${pct}%`
      const d = engine.getDelta(band.id)
      if (deltaRef.current) {
        deltaRef.current.textContent = `${d >= 0 ? '+' : ''}${d.toFixed(1)} dB`
      }
    }
    raf = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(raf)
  }, [engine, band.id, active])

  const threshPct = Math.min(Math.max((band.threshold - METER_MIN) / -METER_MIN, 0), 1) * 100

  return (
    <div className="flex min-w-[130px] flex-1 flex-col gap-1">
      <div className="flex justify-between text-[9px] uppercase tracking-wider text-white/35">
        <span>Band level</span>
        <span ref={deltaRef} className="tabular-nums text-white/70">
          0.0 dB
        </span>
      </div>
      <div className="relative h-2 overflow-hidden rounded-full bg-black/50">
        <div ref={fillRef} className="h-full rounded-full transition-none" style={{ width: '0%', background: color, opacity: 0.6 }} />
        <div
          className="absolute top-0 h-full w-px bg-white/80"
          style={{ left: `${threshPct}%` }}
          title={`Threshold ${band.threshold.toFixed(1)} dB`}
        />
      </div>
      <div className="text-[9px] text-white/25">
        {band.dynMode === 'above' ? 'engages above' : 'engages below'} threshold
      </div>
    </div>
  )
}
