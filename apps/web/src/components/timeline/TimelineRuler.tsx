import { useEffect, useMemo, useRef, useState } from "react";
import { useUIStore } from "../../store/uiStore";
import { useProjectStore } from "../../store/projectStore";
import { C, HEADER_WIDTH, RULER_HEIGHT } from "../../theme";
import {
  Magnet,
  Plus,
  ChevronDown,
} from "lucide-react";
import {
  ARRANGEMENT_GRID_DIVISIONS,
  contentXToTime,
  formatBarBeat,
  getGridStepBeats,
  secondsPerBeat,
  snapTime,
  timeToContentX,
} from "../../utils/musicalTime";
import { TIMELINE_Z } from "../../utils/timelineZ";
import { getArrangementGridLines, pxPerBeat } from "../../utils/musicalGrid";
import { activeAudioEngine } from "../../engine/activeAudioEngine";
import type { TimeSignature } from "../../utils/musicalTime";

type TimelineRulerProps = {
  width: number;
  onAddTrack: () => void;
  snapToGrid: boolean;
  onToggleSnapToGrid: () => void;
};

// Minimum grid-step width (CSS px) for a division to appear in the dropdown.
// Below this threshold lines are too close together to be useful as snap targets.
const MIN_GRID_STEP_PX = 8;

export function TimelineRuler({ width, onAddTrack, snapToGrid, onToggleSnapToGrid }: TimelineRulerProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const wrapRef  = useRef<HTMLDivElement>(null);
  const [gridOpen, setGridOpen] = useState(false);
  const { pixelsPerSecond, scrollX, loopEnabled, loopStart, loopEnd, setLoopStart, setLoopEnd, arrangementGridDivision, setArrangementGridDivision } = useUIStore();
  const { bpm, timeSignature } = useProjectStore((s) => s.project);
  const timeSig: TimeSignature = useMemo(
    () => timeSignature ?? { numerator: 4, denominator: 4 },
    [timeSignature],
  );

  // Divisions whose grid step is wide enough to be meaningful at the current zoom.
  // The currently selected division is always included so it stays accessible.
  const ppb = pxPerBeat(pixelsPerSecond, bpm);
  const visibleDivisions = ARRANGEMENT_GRID_DIVISIONS.filter((div) => {
    const stepPx = getGridStepBeats(div) * ppb;
    return stepPx >= MIN_GRID_STEP_PX || div === arrangementGridDivision;
  });

  useEffect(() => {
    const canvas = canvasRef.current;
    const wrap   = wrapRef.current;
    if (!canvas || !wrap) return;
    let resizeObserver: ResizeObserver | null = null;

    const draw = () => {
      if (!canvas || !wrap) return;
      const W = wrap.offsetWidth || 2000;
      const dpr = window.devicePixelRatio || 1;
      canvas.width  = W * dpr;
      canvas.height = RULER_HEIGHT * dpr;
      canvas.style.width = `${W}px`;
      canvas.style.height = `${RULER_HEIGHT}px`;
      const ctx = canvas.getContext("2d");
      if (!ctx) return;
      ctx.scale(dpr, dpr);

      // Background
      ctx.fillStyle = C.surface;
      ctx.fillRect(0, 0, W, RULER_HEIGHT);

      // Subtle bottom separator line
      ctx.strokeStyle = "rgba(255,255,255,0.06)";
      ctx.lineWidth   = 1;
      ctx.beginPath();
      ctx.moveTo(0, RULER_HEIGHT - 0.5);
      ctx.lineTo(W, RULER_HEIGHT - 0.5);
      ctx.stroke();

      const spb = secondsPerBeat(bpm);

      // Tick geometry: proportional to ruler height for any RULER_HEIGHT value
      const tickSub  = Math.round(RULER_HEIGHT * 0.18); // ~5 px at 28px
      const tickBeat = Math.round(RULER_HEIGHT * 0.46); // ~13 px at 28px
      // bar uses full height

      ctx.textBaseline = "middle";

      const lines = getArrangementGridLines(pixelsPerSecond, bpm, timeSig, scrollX, W, arrangementGridDivision);

      // Draw sub and beat ticks first, bar ticks on top
      for (const line of lines) {
        if (line.level === "bar") continue;
        const { x, level } = line;
        const cx = x + 0.5;

        if (level === "sub") {
          ctx.strokeStyle = "rgba(255,255,255,0.10)";
          ctx.lineWidth   = 1;
          ctx.beginPath();
          ctx.moveTo(cx, RULER_HEIGHT - tickSub);
          ctx.lineTo(cx, RULER_HEIGHT - 1);
          ctx.stroke();
        } else {
          // beat
          ctx.strokeStyle = "rgba(255,255,255,0.18)";
          ctx.lineWidth   = 1;
          ctx.beginPath();
          ctx.moveTo(cx, RULER_HEIGHT - tickBeat);
          ctx.lineTo(cx, RULER_HEIGHT - 1);
          ctx.stroke();
        }
      }

      for (const line of lines) {
        if (line.level !== "bar") continue;
        const cx = line.x + 0.5;
        ctx.strokeStyle = "rgba(255,255,255,0.28)";
        ctx.lineWidth   = 1;
        ctx.beginPath();
        ctx.moveTo(cx, 1);
        ctx.lineTo(cx, RULER_HEIGHT - 1);
        ctx.stroke();
      }

      // Labels — separate pass so text is always on top of ticks
      for (const line of lines) {
        const { x, level, showLabel, beat } = line;
        if (!showLabel) continue;

        const label = formatBarBeat(beat * spb, bpm, timeSig);

        if (level === "bar") {
          ctx.font      = "bold 10px system-ui, sans-serif";
          ctx.fillStyle = "rgba(200,212,224,0.88)";
          ctx.fillText(label, x + 4, RULER_HEIGHT / 2);
        } else {
          // beat / sub labels — dimmer, no bold
          ctx.font      = "10px system-ui, sans-serif";
          ctx.fillStyle = "rgba(107,120,136,0.7)";
          ctx.fillText(label, x + 3, RULER_HEIGHT / 2);
        }
      }
    };

    draw();

    if (wrap) {
      resizeObserver = new ResizeObserver(() => draw());
      resizeObserver.observe(wrap);
    }

    return () => {
      if (resizeObserver) resizeObserver.disconnect();
    };
  }, [bpm, timeSig, pixelsPerSecond, scrollX, width, arrangementGridDivision]);

  const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!wrapRef.current) return;

    const updateTime = (clientX: number) => {
      const rect = wrapRef.current!.getBoundingClientRect();
      const contentX = clientX - rect.left;
      const { scrollX: sx, pixelsPerSecond: pps, snapToGrid, arrangementGridDivision } = useUIStore.getState();
      const rawSeconds = contentXToTime(contentX, pps, sx);

      if (snapToGrid) {
        const spb = secondsPerBeat(bpm);
        activeAudioEngine.seekSeconds(snapTime(rawSeconds, bpm, timeSig, pps * spb, arrangementGridDivision));
      } else {
        activeAudioEngine.seekSeconds(rawSeconds);
      }
    };

    updateTime(e.clientX);

    const onMove = (ev: PointerEvent) => updateTime(ev.clientX);
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  };

  return (
    <div className="flex shrink-0 border-b border-daw-border bg-daw-surface" style={{ height: RULER_HEIGHT }}>
      <div
        className="sticky left-0 flex shrink-0 items-center gap-2 border-r border-daw-border bg-daw-surface px-2 shadow-[8px_0_18px_rgba(0,0,0,0.28)]"
        style={{ width: HEADER_WIDTH, minWidth: HEADER_WIDTH, zIndex: TIMELINE_Z.rulerHeaderLane }}
      >
        <div className="pointer-events-none absolute bottom-0 right-[-12px] top-0 z-0 w-3 bg-gradient-to-r from-daw-surface to-transparent" />
        <span className="relative z-10 min-w-0 flex-1 truncate text-[11px] font-semibold text-daw-text">
          Arrangement
        </span>
        <button
          type="button"
          onClick={onAddTrack}
          title="Add track"
          className="relative z-10 flex h-6 shrink-0 items-center gap-1.5 rounded-md border border-daw-border bg-daw-bg px-2 text-[11px] font-semibold text-daw-dim transition-colors hover:border-daw-border-light hover:bg-daw-surface-high hover:text-daw-text"
        >
          <Plus size={12} />
          Add
        </button>
        <button
          type="button"
          onClick={onToggleSnapToGrid}
          title={snapToGrid ? "Snap to grid: ON [N]" : "Snap to grid: OFF [N]"}
          className={`relative z-10 flex h-6 w-6 shrink-0 items-center justify-center rounded-md border transition-colors ${
            snapToGrid
              ? "border-daw-accent bg-daw-accent text-daw-ink hover:bg-daw-accent-h"
              : "border-daw-border bg-daw-bg text-daw-dim hover:border-daw-border-light hover:bg-daw-surface-high hover:text-daw-text"
          }`}
        >
          <Magnet size={12} />
        </button>
        <div className="relative z-10 shrink-0">
          <button
            type="button"
            onClick={() => setGridOpen((v) => !v)}
            title="Arrangement grid resolution"
            className="flex h-6 items-center gap-1 rounded-md border border-daw-border bg-daw-bg px-1.5 text-[10px] font-medium tabular-nums text-daw-dim transition-colors hover:border-daw-border-light hover:bg-daw-surface-high hover:text-daw-text"
          >
            {arrangementGridDivision === "auto" ? "Auto" : arrangementGridDivision}
            <ChevronDown size={10} />
          </button>
          {gridOpen && (
            <div className="absolute right-0 top-[28px] z-50 w-[164px] rounded-md border border-daw-border bg-daw-surface p-1 shadow-xl">
              <div className="px-2 pb-1 pt-0.5 text-[9px] font-semibold uppercase tracking-[0.14em] text-daw-faint">
                Grid
              </div>
              {/* Auto row — always full-width at top */}
              <button
                type="button"
                onClick={() => { setArrangementGridDivision("auto"); setGridOpen(false); }}
                className={`mb-0.5 h-6 w-full rounded px-2 text-left text-[10px] font-medium transition-colors ${
                  arrangementGridDivision === "auto"
                    ? "bg-daw-accent/20 text-daw-accent"
                    : "text-daw-dim hover:bg-daw-surface-high hover:text-daw-text"
                }`}
              >
                Auto
                <span className="ml-1 text-[9px] opacity-50">adapts to zoom</span>
              </button>
              <div className="grid grid-cols-2 gap-0.5">
                {visibleDivisions.map((division) => (
                  <button
                    key={division}
                    type="button"
                    onClick={() => {
                      setArrangementGridDivision(division);
                      setGridOpen(false);
                    }}
                    className={`h-6 rounded px-2 text-left text-[10px] font-medium tabular-nums transition-colors ${
                      division === arrangementGridDivision
                        ? "bg-daw-accent/20 text-daw-accent"
                        : "text-daw-dim hover:bg-daw-surface-high hover:text-daw-text"
                    }`}
                  >
                    {division}
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>
        <span className="relative z-10 hidden shrink-0 rounded-md border border-daw-border bg-daw-bg px-1.5 py-0.5 text-[10px] text-daw-faint min-[400px]:inline">
          bar.beat
        </span>
      </div>
      <div ref={wrapRef} className="flex-1 overflow-hidden cursor-crosshair relative" onPointerDown={handlePointerDown}>
        <canvas ref={canvasRef} className="block pointer-events-none" />

        {/* Loop Region overlay
            Positions are Math.round'd to align with the integer-pixel ruler ticks.
            box-sizing: border-box keeps the 1 px borders inside the computed width
            so the right edge lands exactly at Math.round(loopEnd * pps - scrollX). */}
        {(() => {
          const lx = timeToContentX(loopStart, pixelsPerSecond, scrollX);
          const rx = timeToContentX(loopEnd,   pixelsPerSecond, scrollX);
          const loopColor = loopEnabled ? "#7bd88f" : "rgba(255,255,255,0.2)";
          return (
            <>
              <div
                className="absolute top-0 bottom-0 pointer-events-none"
                style={{
                  left: lx,
                  width: Math.max(0, rx - lx),
                  boxSizing: "border-box",
                  background: loopEnabled ? "rgba(123,216,143,0.08)" : "rgba(255,255,255,0.02)",
                  borderLeft:  `1px solid ${loopColor}`,
                  borderRight: `1px solid ${loopColor}`,
                  zIndex: TIMELINE_Z.loopRegion,
                }}
              />

              {/* Loop Start Handle — centered on the start border (lx) */}
              <div
                className="absolute top-0 w-3 h-3 cursor-ew-resize flex items-center justify-center"
                style={{ left: lx - 6, zIndex: TIMELINE_Z.loopHandle }}
                onPointerDown={(e) => {
                  e.stopPropagation();
                  const startX = e.clientX;
                  const initialStart = loopStart;
                  const onMove = (ev: PointerEvent) => {
                    let newStart = Math.max(0, initialStart + (ev.clientX - startX) / pixelsPerSecond);
                    if (useUIStore.getState().snapToGrid) {
                      const spb = secondsPerBeat(bpm);
                      newStart = snapTime(newStart, bpm, timeSig, pixelsPerSecond * spb, useUIStore.getState().arrangementGridDivision);
                    }
                    setLoopStart(Math.min(newStart, loopEnd - 0.1));
                  };
                  const onUp = () => { window.removeEventListener("pointermove", onMove); window.removeEventListener("pointerup", onUp); };
                  window.addEventListener("pointermove", onMove);
                  window.addEventListener("pointerup", onUp);
                }}
              >
                <svg width="8" height="8" viewBox="0 0 8 8" className={`drop-shadow ${loopEnabled ? "text-[#7bd88f]" : "text-white/40"}`}>
                  <polygon points="0,0 8,0 8,8" fill="currentColor" />
                </svg>
              </div>

              {/* Loop End Handle — centered on the end border (rx) */}
              <div
                className="absolute top-0 w-3 h-3 cursor-ew-resize flex items-center justify-center"
                style={{ left: rx - 6, zIndex: TIMELINE_Z.loopHandle }}
                onPointerDown={(e) => {
                  e.stopPropagation();
                  const startX = e.clientX;
                  const initialEnd = loopEnd;
                  const onMove = (ev: PointerEvent) => {
                    let newEnd = Math.max(0, initialEnd + (ev.clientX - startX) / pixelsPerSecond);
                    if (useUIStore.getState().snapToGrid) {
                      const spb = secondsPerBeat(bpm);
                      newEnd = snapTime(newEnd, bpm, timeSig, pixelsPerSecond * spb, useUIStore.getState().arrangementGridDivision);
                    }
                    setLoopEnd(Math.max(loopStart + 0.1, newEnd));
                  };
                  const onUp = () => { window.removeEventListener("pointermove", onMove); window.removeEventListener("pointerup", onUp); };
                  window.addEventListener("pointermove", onMove);
                  window.addEventListener("pointerup", onUp);
                }}
              >
                <svg width="8" height="8" viewBox="0 0 8 8" className={`drop-shadow ${loopEnabled ? "text-[#7bd88f]" : "text-white/40"}`}>
                  <polygon points="0,0 8,0 0,8" fill="currentColor" />
                </svg>
              </div>
            </>
          );
        })()}
      </div>
    </div>
  );
}
