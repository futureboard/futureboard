import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { categories, icons, type CategoryId, type Model } from "../data";

type ModelPickerProps = {
  cat: CategoryId;
  models: Model[];
  activeModelId: string;
  onSelect: (id: string) => void;
  onClose: () => void;
};

/**
 * Full-overlay model browser for a signal-chain block — the Helix-Native-
 * style replacement for the old inline chip row (`.model-strip`). One
 * category's models only (the chain's stage slots are fixed; this only
 * changes which model occupies the active one), shown as a searchable grid
 * of name + description tiles instead of a cramped, description-less strip.
 *
 * Portalled to `document.body` for the same reason `BlockMenu` is: it is
 * triggered from inside `.faceplate`, which clips overflow, so an in-flow
 * overlay would be cut off. Structurally it is `DiscardDialog`'s
 * backdrop/panel pattern, plus the Escape/outside-click/initial-focus
 * discipline `DiscardDialog` is missing and `BlockMenu` already has.
 */
export function ModelPicker({
  cat,
  models,
  activeModelId,
  onSelect,
  onClose,
}: ModelPickerProps) {
  const [query, setQuery] = useState("");
  const searchRef = useRef<HTMLInputElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const c = categories[cat];

  useEffect(() => {
    searchRef.current?.focus();
  }, []);

  useEffect(() => {
    const onPointerDown = (e: PointerEvent) => {
      if (panelRef.current?.contains(e.target as Node)) return;
      onClose();
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown, true);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
    };
  }, [onClose]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return models;
    return models.filter(
      (m) => m.name.toLowerCase().includes(q) || m.sub.toLowerCase().includes(q),
    );
  }, [models, query]);

  return createPortal(
    <div className="picker-backdrop" role="presentation">
      <div
        ref={panelRef}
        className="picker-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby="picker-title"
        style={{ ["--mc" as string]: c.color }}
      >
        <div className="picker-head">
          <div className="picker-title-wrap">
            <span className="picker-eyebrow" style={{ color: c.color }}>
              {c.name}
            </span>
            <span id="picker-title" className="picker-title">
              Choose model
            </span>
          </div>
          <button
            type="button"
            className="picker-close"
            aria-label="Close model picker"
            onClick={onClose}
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <line x1="5" y1="5" x2="19" y2="19" />
              <line x1="19" y1="5" x2="5" y2="19" />
            </svg>
          </button>
        </div>

        <input
          ref={searchRef}
          type="text"
          className="picker-search"
          placeholder={`Search ${c.name.toLowerCase()} models…`}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          aria-label={`Search ${c.name} models`}
        />

        <div className="picker-grid" role="listbox" aria-label={`${c.name} models`}>
          {filtered.length === 0 && (
            <div className="picker-empty">No models match “{query}”.</div>
          )}
          {filtered.map((m) => {
            const active = m.id === activeModelId;
            return (
              <button
                key={m.id}
                type="button"
                role="option"
                aria-selected={active}
                className={`picker-tile${active ? " active" : ""}`}
                onClick={() => {
                  onSelect(m.id);
                  onClose();
                }}
              >
                <span className="picker-tile-icon">
                  <svg
                    width="20"
                    height="20"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    dangerouslySetInnerHTML={{ __html: icons[c.node] ?? "" }}
                  />
                </span>
                <span className="picker-tile-text">
                  <span className="picker-tile-name">{m.name}</span>
                  <span className="picker-tile-sub">{m.sub}</span>
                </span>
              </button>
            );
          })}
        </div>
      </div>
    </div>,
    document.body,
  );
}
