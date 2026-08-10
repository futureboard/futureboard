import { useRef } from 'react'

interface Props {
  fileName: string
  hasAudio: boolean
  loading: boolean
  playing: boolean
  loop: boolean
  position: number
  duration: number
  onPlayPause: () => void
  onStop: () => void
  onLoop: (v: boolean) => void
  onSeek: (t: number) => void
  onFile: (f: File) => void
}

function fmtTime(t: number) {
  if (!isFinite(t)) return '0:00'
  const m = Math.floor(t / 60)
  const s = Math.floor(t % 60)
  return `${m}:${String(s).padStart(2, '0')}`
}

export function Transport({
  fileName,
  hasAudio,
  loading,
  playing,
  loop,
  position,
  duration,
  onPlayPause,
  onStop,
  onLoop,
  onSeek,
  onFile,
}: Props) {
  const input = useRef<HTMLInputElement>(null)
  const progress = duration > 0 ? position / duration : 0

  return (
    <div className="flex items-center gap-3 px-3 py-2.5">
      <input
        ref={input}
        type="file"
        accept="audio/*"
        className="hidden"
        onChange={(e) => {
          const f = e.target.files?.[0]
          if (f) onFile(f)
          e.target.value = ''
        }}
      />

      <button
        onClick={onPlayPause}
        disabled={!hasAudio}
        className="neon-solid grid h-8 w-8 place-items-center rounded-full transition disabled:bg-white/10 disabled:text-white/25 disabled:shadow-none"
        title={playing ? 'Pause' : 'Play'}
      >
        {playing ? (
          <svg width={12} height={12} viewBox="0 0 12 12" fill="currentColor">
            <rect x="1.5" y="1" width="3" height="10" rx="1" />
            <rect x="7.5" y="1" width="3" height="10" rx="1" />
          </svg>
        ) : (
          <svg width={12} height={12} viewBox="0 0 12 12" fill="currentColor">
            <path d="M2.5 1.2 10.5 6 2.5 10.8Z" />
          </svg>
        )}
      </button>

      <button
        onClick={onStop}
        disabled={!hasAudio}
        className="glass-pill grid h-8 w-8 place-items-center rounded-full text-white/60 hover:text-white disabled:text-white/20"
        title="Stop"
      >
        <svg width={10} height={10} viewBox="0 0 10 10" fill="currentColor">
          <rect width="10" height="10" rx="1.5" />
        </svg>
      </button>

      <button
        onClick={() => onLoop(!loop)}
        className={`h-8 rounded-full px-3 text-[10px] uppercase tracking-wide transition ${
          loop
            ? 'neon-on'
            : 'glass-pill text-white/45 hover:text-white/80'
        }`}
      >
        Loop
      </button>

      <span className="w-20 text-[10px] tabular-nums text-white/50">
        {fmtTime(position)} / {fmtTime(duration)}
      </span>

      <div
        className="group relative h-6 flex-1 cursor-pointer"
        onPointerDown={(ev) => {
          if (!hasAudio) return
          const rect = ev.currentTarget.getBoundingClientRect()
          onSeek(((ev.clientX - rect.left) / rect.width) * duration)
        }}
      >
        <div className="absolute inset-x-0 top-1/2 h-1 -translate-y-1/2 overflow-hidden rounded-full bg-white/8">
          <div
            className="h-full rounded-full bg-neon/80 shadow-[0_0_12px_rgba(255,77,157,0.6)]"
            style={{ width: `${Math.min(progress, 1) * 100}%` }}
          />
        </div>
        {hasAudio && (
          <div
            className="absolute top-1/2 h-2.5 w-2.5 -translate-x-1/2 -translate-y-1/2 rounded-full bg-mochi opacity-0 shadow-[0_0_10px_rgba(255,77,157,0.9)] transition group-hover:opacity-100"
            style={{ left: `${Math.min(progress, 1) * 100}%` }}
          />
        )}
      </div>

      <button
        onClick={() => input.current?.click()}
        className="glass-pill h-8 rounded-full px-3 text-[10px] uppercase tracking-wide text-white/60 hover:text-white"
      >
        Load file
      </button>

      <span className="max-w-[200px] truncate text-[10px] text-white/40" title={fileName}>
        {loading ? 'Decoding…' : fileName || 'Drop an audio file to preview'}
      </span>
    </div>
  )
}
