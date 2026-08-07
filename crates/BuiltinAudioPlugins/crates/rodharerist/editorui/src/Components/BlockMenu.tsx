import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

export type MenuItem = {
  label: string;
  onSelect: () => void;
  disabled?: boolean;
  /** Renders above this item. */
  separatorBefore?: boolean;
  /** Styles the item as destructive and keeps it away from the safe actions. */
  destructive?: boolean;
};

type BlockMenuProps = {
  /** Accessible name for the trigger, e.g. "Amp block options". */
  label: string;
  items: MenuItem[];
};

/** Keep-out margin from the window edge when clamping the panel. */
const EDGE = 8;
/** Gap between the trigger and the panel. */
const GAP = 4;

/**
 * The three-dot options menu on a signal-chain block.
 *
 * Replaces the bare `×` that used to sit next to the power button: destructive
 * actions are now behind a deliberate second click and separated from the rest
 * of the menu, so a block cannot be dropped from the path by a mis-click.
 *
 * The panel is portalled to `document.body` and positioned from the trigger's
 * measured bounds. It cannot live inside the block: the chain is a horizontal
 * scroller inside a fixed-height row, and both of those ancestors clip
 * overflow — an in-flow panel was cut off a few pixels below the trigger, which
 * left "Remove from Path" unreachable and made the path look add-only.
 *
 * Closes on Escape, on outside pointer-down, and after any selection. It
 * follows the trigger while the chain pans or the window resizes.
 */
export function BlockMenu({ label, items }: BlockMenuProps) {
  const [open, setOpen] = useState(false);
  const btnRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const firstItemRef = useRef<HTMLButtonElement>(null);

  /**
   * Anchor below-left of the trigger, then clamp to the window; flip above if
   * the panel would run off the bottom. Measured, never assumed — a block near
   * the right edge of a panned chain has no room below-right.
   */
  const place = useCallback(() => {
    const btn = btnRef.current;
    const panel = panelRef.current;
    if (!btn || !panel) return;
    const trigger = btn.getBoundingClientRect();
    const size = panel.getBoundingClientRect();

    let left = trigger.left;
    if (left + size.width > window.innerWidth - EDGE) {
      left = window.innerWidth - EDGE - size.width;
    }
    left = Math.max(EDGE, left);

    let top = trigger.bottom + GAP;
    if (top + size.height > window.innerHeight - EDGE) {
      top = trigger.top - size.height - GAP;
    }
    top = Math.max(EDGE, top);

    panel.style.left = `${left}px`;
    panel.style.top = `${top}px`;
  }, []);

  useEffect(() => {
    if (!open) return;

    const onPointerDown = (e: PointerEvent) => {
      const target = e.target as Node;
      // The panel is portalled, so it is not a descendant of the trigger.
      if (btnRef.current?.contains(target)) return;
      if (panelRef.current?.contains(target)) return;
      setOpen(false);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setOpen(false);
      }
    };

    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown, true);
    // Capture, so panning the chain scroller (not just the window) is seen.
    window.addEventListener("scroll", place, true);
    window.addEventListener("resize", place);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("scroll", place, true);
      window.removeEventListener("resize", place);
    };
  }, [open, place]);

  // Position before paint, once the panel has a measured size.
  useLayoutEffect(() => {
    if (!open) return;
    place();
    firstItemRef.current?.focus();
  }, [open, place]);

  return (
    <div className="blk-menu">
      <button
        ref={btnRef}
        type="button"
        className="blk-menu-btn"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={label}
        title={label}
        onPointerDown={(e) => e.stopPropagation()}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((v) => !v);
        }}
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden>
          <circle cx="12" cy="5" r="1.6" />
          <circle cx="12" cy="12" r="1.6" />
          <circle cx="12" cy="19" r="1.6" />
        </svg>
      </button>

      {open &&
        createPortal(
          <div
            ref={panelRef}
            className="blk-menu-panel"
            role="menu"
            aria-label={label}
            onPointerDown={(e) => e.stopPropagation()}
            onClick={(e) => e.stopPropagation()}
          >
            {items.map((item, i) => (
              <div key={item.label} className="blk-menu-row">
                {item.separatorBefore && <div className="blk-menu-sep" role="separator" />}
                <button
                  ref={i === 0 ? firstItemRef : undefined}
                  type="button"
                  role="menuitem"
                  className={`blk-menu-item${item.destructive ? " destructive" : ""}`}
                  disabled={item.disabled}
                  onClick={() => {
                    setOpen(false);
                    item.onSelect();
                  }}
                >
                  {item.label}
                </button>
              </div>
            ))}
          </div>,
          document.body,
        )}
    </div>
  );
}
