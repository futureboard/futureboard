import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { Snapshot } from "../Editor";

type SnapshotDropdownProps = {
  slots: readonly Snapshot[];
  active: number;
  onSelect: (index: number) => void;
  onSaveCurrent: (index: number) => void;
  onRename: (index: number, name: string) => void;
};

const EDGE = 8;
const GAP = 4;

/**
 * Compact snapshot selector: a single LCD-style trigger (current slot's
 * number + name, flanked by step arrows for a quick prev/next recall — the
 * same family as the header's preset nav) that opens a dropdown of all 8
 * slots. Recalls bypass + parameter values only, never model choice or
 * block order (see `Editor.tsx`'s `mergeSnapshotIntoRig`), so switching
 * rides the existing per-parameter smoothing and stays click-free.
 *
 * A slot only changes when explicitly saved into (the row's save icon) —
 * picking a different slot does not silently carry live edits into it,
 * matching a real footswitch bank rather than the A/B compare's
 * auto-sync-on-switch.
 *
 * Positioning/dismiss discipline mirrors `BlockMenu`: portalled to
 * `document.body`, measured and clamped to the viewport, closes on Escape
 * and outside pointerdown, flips above the trigger if there is no room
 * below.
 */
export function SnapshotDropdown({
  slots,
  active,
  onSelect,
  onSaveCurrent,
  onRename,
}: SnapshotDropdownProps) {
  const [open, setOpen] = useState(false);
  const [renaming, setRenaming] = useState<number | null>(null);
  const [draft, setDraft] = useState("");
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const renameRef = useRef<HTMLInputElement>(null);

  const place = useCallback(() => {
    const trigger = triggerRef.current;
    const panel = panelRef.current;
    if (!trigger || !panel) return;
    const t = trigger.getBoundingClientRect();
    const p = panel.getBoundingClientRect();

    let left = t.left;
    if (left + p.width > window.innerWidth - EDGE) left = window.innerWidth - EDGE - p.width;
    left = Math.max(EDGE, left);

    let top = t.bottom + GAP;
    if (top + p.height > window.innerHeight - EDGE) top = t.top - p.height - GAP;
    top = Math.max(EDGE, top);

    panel.style.left = `${left}px`;
    panel.style.top = `${top}px`;
  }, []);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: PointerEvent) => {
      const target = e.target as Node;
      if (triggerRef.current?.contains(target)) return;
      if (panelRef.current?.contains(target)) return;
      setOpen(false);
      setRenaming(null);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        if (renaming !== null) setRenaming(null);
        else setOpen(false);
      }
    };
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("scroll", place, true);
    window.addEventListener("resize", place);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("scroll", place, true);
      window.removeEventListener("resize", place);
    };
  }, [open, place, renaming]);

  useLayoutEffect(() => {
    if (!open) return;
    place();
  }, [open, place]);

  useEffect(() => {
    if (renaming !== null) renameRef.current?.select();
  }, [renaming]);

  const startRename = (index: number) => {
    setDraft(slots[index]?.name ?? String(index + 1));
    setRenaming(index);
  };
  const commitRename = () => {
    if (renaming === null) return;
    const name = draft.trim();
    if (name) onRename(renaming, name);
    setRenaming(null);
  };

  const currentName = slots[active]?.name ?? String(active + 1);
  const step = (dir: number) => onSelect((active + dir + slots.length) % slots.length);

  return (
    <div className="snap-select" role="group" aria-label="Snapshots">
      <button
        type="button"
        className="snap-step"
        onClick={() => step(-1)}
        aria-label="Previous snapshot"
        title="Previous snapshot"
      >
        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4">
          <polyline points="15 18 9 12 15 6" />
        </svg>
      </button>

      <button
        ref={triggerRef}
        type="button"
        className="snap-trigger"
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        title="Choose a snapshot"
      >
        <span className="snap-trigger-badge">{active + 1}</span>
        <span className="snap-trigger-name">{currentName}</span>
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" className="snap-trigger-chevron" aria-hidden>
          <polyline points="6 9 12 15 18 9" />
        </svg>
      </button>

      <button
        type="button"
        className="snap-step"
        onClick={() => step(1)}
        aria-label="Next snapshot"
        title="Next snapshot"
      >
        <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4">
          <polyline points="9 18 15 12 9 6" />
        </svg>
      </button>

      {open &&
        createPortal(
          <div ref={panelRef} className="snap-panel" role="listbox" aria-label="Snapshots">
            <div className="snap-panel-head">
              <span>Snapshots</span>
              <span className="snap-panel-hint">recall bypass + params</span>
            </div>
            {slots.map((slot, index) => {
              const isActive = index === active;
              const isRenaming = renaming === index;
              return (
                <div key={index} className={`snap-row${isActive ? " active" : ""}`} role="option" aria-selected={isActive}>
                  {isRenaming ? (
                    // A plain div, not a button: an <input> is not valid content
                    // inside a <button> and browsers handle the nesting
                    // inconsistently (focus/Enter did not reliably reach the
                    // input). Mutually exclusive with the button below instead.
                    <div className="snap-row-main renaming">
                      <span className="snap-row-num">{index + 1}</span>
                      <input
                        ref={renameRef}
                        type="text"
                        className="snap-row-rename"
                        value={draft}
                        maxLength={20}
                        onChange={(e) => setDraft(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") commitRename();
                          if (e.key === "Escape") setRenaming(null);
                        }}
                        onBlur={commitRename}
                      />
                    </div>
                  ) : (
                    <button
                      type="button"
                      className="snap-row-main"
                      onClick={() => {
                        onSelect(index);
                        setOpen(false);
                      }}
                    >
                      <span className="snap-row-num">{index + 1}</span>
                      <span className="snap-row-name">{slot.name}</span>
                    </button>
                  )}
                  <div className="snap-row-actions">
                    <button
                      type="button"
                      className="snap-row-icon"
                      title={`Rename snapshot ${index + 1}`}
                      aria-label={`Rename snapshot ${index + 1}`}
                      onClick={() => startRename(index)}
                    >
                      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                        <path d="M12 20h9" />
                        <path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4Z" />
                      </svg>
                    </button>
                    <button
                      type="button"
                      className="snap-row-icon"
                      title={`Save current settings into slot ${index + 1}`}
                      aria-label={`Save current settings into slot ${index + 1}`}
                      onClick={() => onSaveCurrent(index)}
                    >
                      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                        <path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2Z" />
                        <polyline points="17 21 17 13 7 13 7 21" />
                        <polyline points="7 3 7 8 15 8" />
                      </svg>
                    </button>
                  </div>
                </div>
              );
            })}
          </div>,
          document.body,
        )}
    </div>
  );
}
