import { useRef, useEffect } from "react";
import {
  clamp,
  normalizeNuaDeeParams,
  serializeNuaDeeParams,
  satDrive,
  type NuaDeeParams,
} from "../Core";

// ── Theme ─────────────────────────────────────────────────────────────────────

const ACCENT     = "#4ade80";
const ACCENT_RGB = "74,222,128";

// ── Types ─────────────────────────────────────────────────────────────────────

type Props = {
  params: Record<string, number | string | boolean>;
  enabled: boolean;
  onParamsChange: (patch: Record<string, number | string | boolean>) => void;
  onToggleEnabled: () => void;
  onReset: () => void;
};

// ── SVG arc helper ────────────────────────────────────────────────────────────

function describeArc(cx: number, cy: number, r: number, startDeg: number, endDeg: number): string {
  const toRad = (d: number) => ((d - 90) * Math.PI) / 180;
  const x1 = cx + r * Math.cos(toRad(startDeg));
  const y1 = cy + r * Math.sin(toRad(startDeg));
  const x2 = cx + r * Math.cos(toRad(endDeg));
  const y2 = cy + r * Math.sin(toRad(endDeg));
  const large = endDeg - startDeg > 180 ? 1 : 0;
  return `M ${x1} ${y1} A ${r} ${r} 0 ${large} 1 ${x2} ${y2}`;
}

// ── Knob ─────────────────────────────────────────────────────────────────────

function Knob({
  label, display, unit, value, onDrag, accent = ACCENT, big = false,
}: {
  label: string;
  display: string;
  unit?: string;
  value: number; // 0..1 normalised
  onDrag: (delta: number) => void;
  accent?: string;
  big?: boolean;
}) {
  const drag = useRef<{ y: number } | null>(null);
  const sz  = big ? 70 : 58;
  const cx  = sz / 2;
  const cy  = sz / 2;
  const r   = big ? 25 : 20;
  const START = 220, SWEEP = 280;
  const angle = START + value * SWEEP;
  const toRad = (d: number) => (d * Math.PI) / 180;

  return (
    <div
      className="flex cursor-ns-resize select-none flex-col items-center gap-[3px]"
      onPointerDown={(e) => { drag.current = { y: e.clientY }; e.currentTarget.setPointerCapture(e.pointerId); }}
      onPointerMove={(e) => { const s = drag.current; if (!s) return; onDrag(s.y - e.clientY); drag.current = { y: e.clientY }; }}
      onPointerUp={(e)   => { drag.current = null; e.currentTarget.releasePointerCapture(e.pointerId); }}
    >
      <svg width={sz} height={sz} viewBox={`0 0 ${sz} ${sz}`}>
        {/* Track arc */}
        <path d={describeArc(cx, cy, r, START, START + SWEEP)}
          fill="none" stroke="rgba(255,255,255,0.07)" strokeWidth="3" strokeLinecap="round" />
        {/* Value arc */}
        <path d={describeArc(cx, cy, r, START, angle)}
          fill="none" stroke={accent} strokeWidth="3" strokeLinecap="round"
          style={{ filter: `drop-shadow(0 0 3px ${accent}88)` }} />
        {/* Center cap */}
        <circle cx={cx} cy={cy} r={r - 13}
          fill="#121810" stroke="rgba(255,255,255,0.07)" strokeWidth="0.75" />
        {/* Indicator tick */}
        <line
          x1={cx} y1={cy}
          x2={cx + (r - 15) * Math.cos(toRad(angle - 90))}
          y2={cy + (r - 15) * Math.sin(toRad(angle - 90))}
          stroke={accent} strokeWidth="1.5" strokeLinecap="round" />
      </svg>

      <div className="text-center leading-none">
        <div className="tabular-nums font-semibold" style={{ fontSize: "11px", color: "#c0d0a0" }}>{display}</div>
        {unit && <div style={{ fontSize: "7px", color: "#485840", marginTop: "1px" }}>{unit}</div>}
      </div>
      <div style={{ fontSize: "7.5px", color: "#566448", letterSpacing: "0.08em", textTransform: "uppercase" }}>
        {label}
      </div>
    </div>
  );
}

// ── Saturation curve canvas ───────────────────────────────────────────────────

function SaturationCurve({ saturation, gain, active }: { saturation: number; gain: number; active: boolean }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const dpr  = devicePixelRatio || 1;
    const rect = canvas.getBoundingClientRect();
    const w    = Math.max(1, rect.width);
    const h    = Math.max(1, rect.height);
    canvas.width  = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    ctx.save();
    ctx.scale(dpr, dpr);

    // Background
    ctx.fillStyle = "#0c1008";
    ctx.fillRect(0, 0, w, h);

    const pad = 20;
    const pw  = w - pad * 2;
    const ph  = h - pad * 2;
    const midX = pad + pw / 2;
    const midY = pad + ph / 2;

    // Faint grid
    ctx.lineWidth = 0.5;
    ctx.strokeStyle = "rgba(255,255,255,0.04)";
    for (const v of [-0.5, 0.5]) {
      ctx.beginPath(); ctx.moveTo(midX + v * pw, pad);       ctx.lineTo(midX + v * pw, pad + ph); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(pad,            midY + v * ph); ctx.lineTo(pad + pw, midY + v * ph); ctx.stroke();
    }

    // Centre axes
    ctx.strokeStyle = "rgba(255,255,255,0.10)";
    ctx.lineWidth   = 0.75;
    ctx.beginPath(); ctx.moveTo(pad, midY);  ctx.lineTo(pad + pw, midY);  ctx.stroke();
    ctx.beginPath(); ctx.moveTo(midX, pad);  ctx.lineTo(midX, pad + ph);  ctx.stroke();

    // Linear reference diagonal
    ctx.strokeStyle = "rgba(255,255,255,0.07)";
    ctx.lineWidth = 1;
    ctx.setLineDash([3, 4]);
    ctx.beginPath();
    ctx.moveTo(pad, pad + ph);
    ctx.lineTo(pad + pw, pad);
    ctx.stroke();
    ctx.setLineDash([]);

    // Tanh curve
    const drive   = satDrive(saturation);
    const gainLin = Math.pow(10, gain / 20);
    const STEPS   = 300;

    const buildPath = () => {
      ctx.beginPath();
      for (let i = 0; i <= STEPS; i++) {
        const xNorm  = (i / STEPS) * 2 - 1; // -1..1
        const output = Math.tanh(xNorm * gainLin * drive);
        const px     = pad + (xNorm + 1) * 0.5 * pw;
        const py     = midY - output * ph * 0.5;
        if (i === 0) ctx.moveTo(px, py);
        else         ctx.lineTo(px, py);
      }
    };

    const curveColor = active ? ACCENT : "rgba(80,100,60,0.5)";

    // Soft glow pass
    buildPath();
    ctx.strokeStyle = active ? `rgba(${ACCENT_RGB},0.13)` : "rgba(60,80,40,0.08)";
    ctx.lineWidth   = 10;
    ctx.stroke();

    // Main line
    buildPath();
    ctx.strokeStyle  = curveColor;
    ctx.lineWidth    = 1.75;
    ctx.shadowColor  = active ? `rgba(${ACCENT_RGB},0.55)` : "transparent";
    ctx.shadowBlur   = 8;
    ctx.stroke();
    ctx.shadowBlur   = 0;

    // Drive label
    ctx.font         = `500 8px system-ui,sans-serif`;
    ctx.fillStyle    = active ? `rgba(${ACCENT_RGB},0.5)` : "rgba(80,100,60,0.35)";
    ctx.textAlign    = "left";
    ctx.textBaseline = "top";
    ctx.fillText(`×${drive.toFixed(1)}`, pad + 4, pad + 4);

    // Corner label
    ctx.font         = "400 7px system-ui,sans-serif";
    ctx.fillStyle    = "rgba(80,110,55,0.3)";
    ctx.textAlign    = "center";
    ctx.textBaseline = "bottom";
    ctx.fillText("CURVE", midX, pad + ph - 2);

    ctx.restore();
  }, [saturation, gain, active]);

  return <canvas ref={canvasRef} className="h-full w-full" style={{ display: "block" }} />;
}

// ── Drive meter (segmented bar) ───────────────────────────────────────────────

function DriveMeter({ saturation }: { saturation: number }) {
  const SEG = 20;
  return (
    <div className="flex flex-1 items-center gap-[2px]">
      {Array.from({ length: SEG }, (_, i) => {
        const threshold = (i + 1) / SEG;
        const active    = saturation / 100 >= threshold;
        const hot       = threshold > 0.75;
        return (
          <div
            key={i}
            style={{
              flex: 1,
              height: "7px",
              borderRadius: "1px",
              background: active
                ? hot
                  ? `rgba(251,191,36,${0.55 + threshold * 0.45})`
                  : `rgba(${ACCENT_RGB},${0.4 + threshold * 0.55})`
                : "rgba(255,255,255,0.04)",
              boxShadow: active && threshold > 0.9 ? "0 0 4px rgba(251,191,36,0.45)" : "none",
            }}
          />
        );
      })}
    </div>
  );
}

// ── Mini bar (boost / mix readout) ───────────────────────────────────────────

function MiniBar({ label, value }: { label: string; value: number }) {
  return (
    <div className="flex flex-col items-center gap-[3px]">
      <span style={{ fontSize: "6.5px", color: "#3a4a2a", textTransform: "uppercase", letterSpacing: "0.1em" }}>{label}</span>
      <div style={{ width: "28px", height: "4px", borderRadius: "2px", background: "#0a0c08", overflow: "hidden", border: "1px solid rgba(255,255,255,0.05)" }}>
        <div style={{ height: "100%", width: `${value}%`, background: `rgba(${ACCENT_RGB},0.5)`, borderRadius: "2px" }} />
      </div>
    </div>
  );
}

// ── Main editor ───────────────────────────────────────────────────────────────

export function NuaDeeEditor({ params, enabled, onParamsChange, onToggleEnabled, onReset }: Props) {
  const model = normalizeNuaDeeParams(params);
  const drive = satDrive(model.saturation);

  const update = (patch: Partial<NuaDeeParams>) => {
    onParamsChange(serializeNuaDeeParams({ ...model, ...patch }));
  };

  const active = enabled && model.power;

  return (
    <div
      className="flex h-full max-h-[380px] min-h-[260px] w-[700px] max-w-[1100px] flex-col overflow-hidden rounded-[6px] text-[11px]"
      style={{
        background: "#111510",
        border: "1px solid rgba(255,255,255,0.09)",
        boxShadow: "0 4px 28px rgba(0,0,0,0.6), 0 1px 0 rgba(255,255,255,0.04) inset",
      }}
    >
      {/* ── Header ── */}
      <div
        className="flex h-8 shrink-0 items-center gap-3 px-3"
        style={{
          background: "linear-gradient(180deg,#181d11 0%,#141710 100%)",
          borderBottom: "1px solid rgba(255,255,255,0.07)",
        }}
      >
        {/* Power LED */}
        <button
          type="button"
          onClick={onToggleEnabled}
          title={enabled ? "Bypass NuaDee" : "Enable NuaDee"}
          className="h-[13px] w-[13px] shrink-0 rounded-full transition-all"
          style={enabled
            ? { background: ACCENT, boxShadow: `0 0 8px rgba(${ACCENT_RGB},0.75)`, border: `1px solid rgba(${ACCENT_RGB},0.5)` }
            : { background: "#1c2014", border: "1px solid rgba(255,255,255,0.12)" }
          }
        />
        <span className="font-semibold tracking-[0.07em]" style={{ color: "#c8d8a8", fontSize: "11.5px" }}>NUADEE</span>
        <span className="text-[8.5px] uppercase tracking-[0.18em]" style={{ color: "rgba(160,200,120,0.32)" }}>Tape Saturation</span>

        <div className="ml-auto flex items-center gap-1.5">
          {/* Drive readout chip */}
          <span
            className="tabular-nums rounded px-2 py-[3px]"
            style={{
              fontSize: "9px",
              color: active ? `rgba(${ACCENT_RGB},0.7)` : "#3a4a2a",
              background: "#0e120a",
              border: "1px solid rgba(255,255,255,0.06)",
            }}
          >
            {active ? `${drive.toFixed(2)}× drive` : "BYPASSED"}
          </span>
          <button
            type="button"
            onClick={onReset}
            className="rounded px-2 py-[3px]"
            style={{ fontSize: "10px", color: "#7a8a6a", background: "#161b10", border: "1px solid rgba(255,255,255,0.07)" }}
            onMouseEnter={(e) => { (e.currentTarget as HTMLButtonElement).style.color = "#a0c080"; }}
            onMouseLeave={(e) => { (e.currentTarget as HTMLButtonElement).style.color = "#7a8a6a"; }}
          >
            Reset
          </button>
        </div>
      </div>

      {/* ── Body ── */}
      <div className="flex min-h-0 flex-1">

        {/* Left: saturation curve canvas */}
        <div
          className="relative w-[178px] shrink-0"
          style={{ borderRight: "1px solid rgba(255,255,255,0.06)", background: "#0c1008" }}
        >
          {!active && (
            <div className="pointer-events-none absolute inset-0 z-10" style={{ background: "rgba(6,8,5,0.52)" }} />
          )}
          <SaturationCurve saturation={model.saturation} gain={model.gain} active={active} />
          <div
            className="absolute bottom-0 left-0 right-0 px-2 py-[3px]"
            style={{ background: "rgba(0,0,0,0.45)", borderTop: "1px solid rgba(255,255,255,0.04)" }}
          >
            <span style={{ fontSize: "7px", color: "rgba(90,120,60,0.4)", textTransform: "uppercase", letterSpacing: "0.09em" }}>
              in → gain → sat → out
            </span>
          </div>
        </div>

        {/* Right: knobs + bottom strip */}
        <div className="flex min-w-0 flex-1 flex-col">

          {/* Knobs row */}
          <div className="flex flex-1 items-center justify-around px-5 py-3">
            <Knob
              label="Gain"
              display={`${model.gain >= 0 ? "+" : ""}${model.gain.toFixed(1)}`}
              unit="dB"
              value={(model.gain + 24) / 48}
              onDrag={(d) => update({ gain: clamp(model.gain + d * 0.25, -24, 24) })}
            />
            <Knob
              label="Boost"
              display={model.boost.toFixed(0)}
              unit="%"
              value={model.boost / 100}
              onDrag={(d) => update({ boost: clamp(model.boost + d * 0.6, 0, 100) })}
            />
            {/* Saturation — slightly larger, it's the main control */}
            <Knob
              label="Saturation"
              display={model.saturation.toFixed(0)}
              unit="%"
              value={model.saturation / 100}
              onDrag={(d) => update({ saturation: clamp(model.saturation + d * 0.6, 0, 100) })}
              big
            />
            <Knob
              label="Mix"
              display={model.mix.toFixed(0)}
              unit="%"
              value={model.mix / 100}
              onDrag={(d) => update({ mix: clamp(model.mix + d * 0.6, 0, 100) })}
            />
            <Knob
              label="Out"
              display={`${model.out >= 0 ? "+" : ""}${model.out.toFixed(1)}`}
              unit="dB"
              value={(model.out + 24) / 36}
              onDrag={(d) => update({ out: clamp(model.out + d * 0.2, -24, 12) })}
            />
          </div>

          {/* Bottom strip: drive meter + mini readouts */}
          <div
            className="flex h-[38px] shrink-0 items-center gap-3 px-4"
            style={{ background: "#0d1009", borderTop: "1px solid rgba(255,255,255,0.05)" }}
          >
            <span style={{ fontSize: "7.5px", color: "#485840", textTransform: "uppercase", letterSpacing: "0.1em", whiteSpace: "nowrap" }}>
              Drive
            </span>
            <DriveMeter saturation={model.saturation} />
            <span className="tabular-nums" style={{ fontSize: "8px", color: `rgba(${ACCENT_RGB},0.55)`, minWidth: "34px", textAlign: "right" }}>
              ×{drive.toFixed(1)}
            </span>

            {/* Separator */}
            <div style={{ width: "1px", height: "16px", background: "rgba(255,255,255,0.06)" }} />

            <MiniBar label="Boost" value={model.boost} />
            <MiniBar label="Mix"   value={model.mix}   />
          </div>
        </div>
      </div>
    </div>
  );
}
