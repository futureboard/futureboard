import { CONTROL, Dropdown, Menu, MenuDivider, MenuItem } from "./ui/Menu";
import { PresetMenu } from "./PresetMenu";
import { Knob } from "./Knob";
import type { ChannelView } from "../dsp/bands";
import type { Snapshot } from "../state/presets";
import Logo from "../assets/logo.svg";

interface Props {
  channelView: ChannelView;
  dbRange: number;
  outputGain: number;
  bypassed: boolean;
  slot: "A" | "B";
  otherSlotFilled: boolean;
  getSnapshot: () => Snapshot;
  onLoadSnapshot: (snap: Snapshot, name: string) => void;
  onSwitchSlot: () => void;
  onCopyToOther: () => void;
  onChannelView: (v: ChannelView) => void;
  onDbRange: (r: number) => void;
  onOutputGain: (g: number) => void;
  onBypass: (b: boolean) => void;
  onReset: () => void;
}

const RANGES = [6, 12, 18, 30];

const VIEWS: { value: ChannelView; label: string }[] = [
  { value: "all", label: "Stereo" },
  { value: "mid", label: "Mid" },
  { value: "side", label: "Side" },
];

function Divider() {
  return <div className="h-5 w-px shrink-0 bg-white/12" />;
}

/**
 * Moves the specular highlight to the pointer. Written straight to the style
 * attribute rather than through state — this fires on every mouse move, and a
 * re-render per frame would cost far more than the paint.
 */
function trackSheen(ev: React.PointerEvent<HTMLElement>) {
  const el = ev.currentTarget;
  const r = el.getBoundingClientRect();
  el.style.setProperty("--gx", `${ev.clientX - r.left}px`);
  el.style.setProperty("--gy", `${ev.clientY - r.top}px`);
}

export function Header({
  channelView,
  dbRange,
  outputGain,
  bypassed,
  slot,
  otherSlotFilled,
  getSnapshot,
  onLoadSnapshot,
  onSwitchSlot,
  onCopyToOther,
  onChannelView,
  onDbRange,
  onOutputGain,
  onBypass,
  onReset,
}: Props) {
  return (
    <header
      onPointerMove={trackSheen}
      onPointerEnter={(ev) =>
        ev.currentTarget.style.setProperty("--sheen", "1")
      }
      onPointerLeave={(ev) =>
        ev.currentTarget.style.setProperty("--sheen", "0")
      }
      className="glass glass-sheen relative z-20 flex min-h-[54px] flex-wrap items-center gap-x-2.5 gap-y-2 rounded-[22px] px-2.5 py-2"
    >
      {/* brand */}
      <div className="flex shrink-0 items-center gap-2.5 pl-1">
        <div className="hidden items-baseline gap-1.5 sm:flex">
          <img src={Logo} alt="EQUZ Free" className="h-4 w-auto" />
        </div>
      </div>

      <Divider />

      {/* A/B compare */}
      <div className="flex shrink-0 items-center gap-1.5">
        <div className="glass-pill flex h-8 items-center rounded-full p-0.5">
          {(["A", "B"] as const).map((s) => (
            <button
              key={s}
              type="button"
              onClick={() => s !== slot && onSwitchSlot()}
              title={`Slot ${s}${otherSlotFilled || s === slot ? "" : " (empty)"} — X to swap`}
              className={`h-7 w-7 rounded-full text-[11px] font-semibold transition ${
                slot === s
                  ? "neon-solid"
                  : "text-white/40 hover:bg-white/10 hover:text-white/75"
              }`}
            >
              {s}
            </button>
          ))}
        </div>
        <button
          type="button"
          onClick={onCopyToOther}
          title={`Copy slot ${slot} into ${slot === "A" ? "B" : "A"}`}
          className={`${CONTROL} font-medium`}
        >
          {slot} → {slot === "A" ? "B" : "A"}
        </button>
      </div>

      <PresetMenu getSnapshot={getSnapshot} onLoad={onLoadSnapshot} />

      {/* view options */}
      <div className="ml-auto flex shrink-0 items-center gap-2.5">
        <Dropdown
          label="View"
          value={channelView}
          options={VIEWS.map((v) => ({ value: v.value, label: v.label }))}
          onChange={(v) => onChannelView(v as ChannelView)}
          align="end"
        />
        <Dropdown
          label="Range"
          value={String(dbRange)}
          options={RANGES.map((r) => ({
            value: String(r),
            label: `± ${r} dB`,
          }))}
          onChange={(v) => onDbRange(Number(v))}
          align="end"
        />

        <Divider />

        {/* output gain */}
        {/* Built from the pill primitives rather than CONTROL — this one needs its
            own padding and no gap, and Tailwind conflicts don't resolve by order. */}
        <div
          className="glass-pill flex h-8 items-center rounded-full pl-1.5 pr-3"
          title="Output gain — drag the knob, double-click to reset"
        >
          <Knob
            label="Out"
            value={outputGain}
            min={-24}
            max={12}
            defaultValue={0}
            size={24}
            layout="inline"
            format={(v) => `${v > 0 ? "+" : ""}${v.toFixed(1)} dB`}
            onChange={onOutputGain}
          />
        </div>

        <button
          type="button"
          onClick={() => onBypass(!bypassed)}
          title="Bypass the whole EQ (B)"
          className={`flex h-8 items-center gap-1.5 rounded-full px-3 text-[11px] font-medium transition ${
            bypassed
              ? "neon-on"
              : "glass-pill text-white/55 hover:text-white/85"
          }`}
        >
          <svg width={12} height={12} viewBox="0 0 12 12" fill="none">
            <path
              d="M6 1.5v4"
              stroke="currentColor"
              strokeWidth={1.7}
              strokeLinecap="round"
            />
            <path
              d="M3.2 3.4a3.6 3.6 0 1 0 5.6 0"
              stroke="currentColor"
              strokeWidth={1.7}
              strokeLinecap="round"
            />
          </svg>
          Bypass
        </button>

        <Menu
          align="end"
          panelClass="w-[196px]"
          title="More"
          triggerClass={`${CONTROL} w-8 justify-center px-0`}
          trigger={() => (
            <svg
              width={14}
              height={14}
              viewBox="0 0 14 14"
              className="text-white/60"
            >
              <circle cx="3" cy="7" r="1.2" fill="currentColor" />
              <circle cx="7" cy="7" r="1.2" fill="currentColor" />
              <circle cx="11" cy="7" r="1.2" fill="currentColor" />
            </svg>
          )}
        >
          {(close) => (
            <>
              <MenuItem
                onClick={() => {
                  onReset();
                  close();
                }}
                danger
              >
                Reset to flat
              </MenuItem>
              <MenuDivider />
              <div className="px-2.5 py-1.5 text-[10px] leading-relaxed text-white/35">
                <div>Click display — add band</div>
                <div>Scroll handle — Q / slope</div>
                <div>Right-drag handle — solo</div>
                <div>Space — play · B — bypass</div>
                <div>X — swap A/B · Esc — deselect</div>
              </div>
            </>
          )}
        </Menu>
      </div>
    </header>
  );
}
