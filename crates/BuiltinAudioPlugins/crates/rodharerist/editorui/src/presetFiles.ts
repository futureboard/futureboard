// Preset file format — owned entirely by the editor. Native stores the bytes
// verbatim under Documents/Futureboard Studio/Rodhareist/Presets and only
// sanitizes the leaf file name; everything about the JSON shape lives here.

import {
  presetsData,
  type CategoryId,
} from "./data";
import type { RigSnapshot, Snapshot } from "./Editor";

export const PRESET_FILE_FORMAT = "rodhareist-preset";
export const PRESET_FILE_VERSION = 1;

/**
 * A persisted snapshot bank. Optional on `PresetFile` — additive to v1, so
 * older files (and older builds reading a newer file) round-trip fine: an
 * absent/malformed block just means the editor reseeds a fresh bank from
 * `snapshot` (see `Editor.tsx`'s `normalizeSnapshotBank`).
 */
export type SerializedSnapshotBank = {
  active: number;
  slots: readonly Snapshot[];
};

export type PresetFile = {
  format: typeof PRESET_FILE_FORMAT;
  version: number;
  id: string;
  name: string;
  category: CategoryId;
  snapshot: RigSnapshot;
  snapshots?: SerializedSnapshotBank;
};

export function serializePreset(
  id: string,
  name: string,
  category: CategoryId,
  snapshot: RigSnapshot,
  snapshots?: SerializedSnapshotBank,
): string {
  const file: PresetFile = {
    format: PRESET_FILE_FORMAT,
    version: PRESET_FILE_VERSION,
    id,
    name,
    category,
    snapshot,
    ...(snapshots ? { snapshots } : {}),
  };
  return JSON.stringify(file, null, 2);
}

/**
 * Parse a preset file's text. Returns `null` for anything that is not a
 * valid v1 preset — corrupt file, foreign JSON, future major version.
 */
export function parsePresetFile(text: string): PresetFile | null {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return null;
  }
  if (!raw || typeof raw !== "object") return null;
  const file = raw as Partial<PresetFile>;
  if (file.format !== PRESET_FILE_FORMAT) return null;
  if (typeof file.version !== "number" || file.version > PRESET_FILE_VERSION) return null;
  if (typeof file.id !== "string" || typeof file.name !== "string") return null;
  const snap = file.snapshot;
  if (!snap || typeof snap !== "object") return null;
  if (!Array.isArray(snap.pathOrder)) return null;
  if (!snap.parameters || typeof snap.parameters !== "object") return null;
  if (!snap.stageModels || typeof snap.stageModels !== "object") return null;
  if (!snap.globals || typeof snap.globals !== "object") return null;

  // `snapshots` is optional and additive: a missing or structurally invalid
  // block is dropped rather than rejecting an otherwise-valid preset file —
  // the rig itself is still good, and the editor reseeds a fresh bank.
  const bank = file.snapshots;
  const validBank =
    bank &&
    typeof bank === "object" &&
    typeof bank.active === "number" &&
    Array.isArray(bank.slots);
  return { ...file, snapshots: validBank ? bank : undefined } as PresetFile;
}

/**
 * Client-side mirror of native's leaf-name sanitizer, so the name we request
 * is the name that lands on disk (native appends `.json` too, but predicting
 * it keeps the UI list-refresh in sync).
 */
export function presetFileName(id: string, name: string): string {
  const cleaned = `${id} ${name}`
    .replace(/[/\\:.\x00-\x1f]/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .slice(0, 120);
  return `${cleaned || "Preset"}.json`;
}

/**
 * Factory seeding: called once when the first `fileList(presets)` arrives
 * empty. `buildSnapshot` is the editor's `factorySnapshot`; `write` posts a
 * single file. Returns how many were written (for the follow-up re-list).
 */
export function seedFactoryPresets(
  buildSnapshot: (id: string) => RigSnapshot | null,
  write: (fileName: string, content: string) => void,
): number {
  let written = 0;
  for (const preset of presetsData) {
    const snapshot = buildSnapshot(preset.id);
    if (!snapshot) continue;
    write(
      presetFileName(preset.id, preset.name),
      serializePreset(preset.id, preset.name, preset.category, snapshot),
    );
    written += 1;
  }
  return written;
}
